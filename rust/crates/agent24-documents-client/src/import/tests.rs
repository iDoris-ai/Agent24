// Ports of apps/desktop/src/main/documents-import.test.ts (task brief, PR-1):
// the same fake OS, the same cases, one Rust test per TS case.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::response::{ApiError, Response};
use crate::transport::{Reply, Request, Transport};

const HELLO_SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

/// Keys computed by the desktop's `keyFor` (node), so a file imported by an
/// older desktop and by this code reaches the same upload.
#[test]
fn keys_match_the_desktop_byte_for_byte() {
    assert_eq!(
        key_for(HELLO_SHA, 5, Some("a.pdf"), None),
        "desktop-3d4c3132c868f574e9c3ebcba333aacedc033d4e51fd3a48df8d4314d39f9a21"
    );
    assert_eq!(
        key_for(HELLO_SHA, 5, None, None),
        "desktop-ad93bda1929e6e857b91c21ca291ed5e6b6c6a4aae66eace08e58554e95cc02a"
    );
    assert_eq!(
        key_for(
            HELLO_SHA,
            5,
            Some("报告 é.pdf"),
            Some("upl_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        ),
        "desktop-151a999cb6d3ba733b667b6a343823f51b85ec0ae1e87799ef113e3a183fbb34"
    );
}

#[test]
fn only_a_name_the_contract_allows_is_sent() {
    assert_eq!(
        filename_of(Path::new("/tmp/x/report.pdf")).as_deref(),
        Some("report.pdf")
    );
    assert_eq!(
        filename_of(Path::new(&"a".repeat(255))).as_deref(),
        Some("a".repeat(255).as_str())
    );
    assert_eq!(filename_of(Path::new(&"a".repeat(256))), None);
    assert_eq!(filename_of(Path::new("/tmp/back\\slash.pdf")), None);
    assert_eq!(filename_of(Path::new("/")), None);
}

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FILE_NO: AtomicU64 = AtomicU64::new(0);

const LATER: &str = "2999-01-01T00:00:00.000Z";
const PAST: &str = "2000-01-01T00:00:00.000Z";

async fn begin() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = LOCK.lock().await;
    forget_chains();
    guard
}

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn reply(status: u16, body: Value) -> Reply {
    Reply::Http {
        status,
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn kernel_timeout() -> Reply {
    reply(
        504,
        json!({"error": {"code": "upstream_timeout", "message": "module took too long"}}),
    )
}

fn local_timeout() -> Reply {
    Reply::Failed {
        status: 504,
        error: ApiError::new("backend_timeout", "outcome unknown"),
    }
}

fn err_reply(status: u16, code: &str, message: &str) -> Reply {
    reply(status, json!({"error": {"code": code, "message": message}}))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn kind_of(path: &str) -> &'static str {
    if path.ends_with("/uploads") {
        "create"
    } else if path.ends_with("/chunks") {
        "chunk"
    } else {
        "import"
    }
}

fn chunk_upload_id(path: &str) -> Option<&str> {
    path.split('/').nth(5)
}

fn is_past(expires_at: &str) -> bool {
    let Some(exp) = unix_secs(expires_at) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    exp <= now
}

fn unix_secs(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || *b.last()? != b'Z'
    {
        return None;
    }
    let y: i32 = text[0..4].parse().ok()?;
    let mo: u32 = text[5..7].parse().ok()?;
    let d: u32 = text[8..10].parse().ok()?;
    let h: u32 = text[11..13].parse().ok()?;
    let mi: u32 = text[14..16].parse().ok()?;
    let se: u32 = text[17..19].parse().ok()?;
    let days = days_from_civil(y, mo, d)?;
    let secs = days * 86400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(se);
    u64::try_from(secs).ok()
}

fn days_from_civil(mut y: i32, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if m <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * u64::from(mp) + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(i64::from(era) * 146097 + doe as i64 - 719468)
}

struct Upload {
    id: String,
    request: String,
    total: u64,
    sha256: String,
    bytes: Vec<u8>,
    chunks: std::collections::HashMap<u64, String>,
    status: String,
    expires_at: String,
}

struct Job {
    job_id: String,
    kind: String,
    status: String,
}

struct FakeState {
    era: char,
    by_key: std::collections::HashMap<String, usize>,
    ids: std::collections::HashMap<String, usize>,
    uploads: Vec<Upload>,
    jobs: std::collections::HashMap<String, Job>,
    calls: Vec<Call>,
    counts: std::collections::HashMap<String, usize>,
}

#[derive(Clone)]
struct Call {
    kind: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Snap {
    status: String,
    bytes: Vec<u8>,
}

enum Step {
    Now(Reply),
    ThroughDrop(Reply),
}

fn view(up: &Upload) -> Value {
    json!({
        "upload_id": up.id,
        "total_size": up.total,
        "sha256": up.sha256,
        "received": up.bytes.len(),
        "status": up.status,
        "expires_at": up.expires_at,
    })
}

fn job_json(job: &Job) -> Value {
    json!({"job_id": job.job_id, "kind": job.kind, "status": job.status})
}

fn do_create(state: &mut FakeState, request: &Request) -> Step {
    let raw = request.body.clone().unwrap_or_default();
    let text = String::from_utf8(raw.clone()).unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap();
    let total_size = parsed
        .get("total_size")
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN);
    if total_size < 1.0 {
        return Step::Now(err_reply(400, "invalid_request", "total_size"));
    }
    let key = header(&request.headers, "Idempotency-Key").to_owned();
    if let Some(&idx) = state.by_key.get(&key) {
        if state.uploads[idx].request != text {
            return Step::Now(err_reply(422, "idempotency_key_reused", "x"));
        }
        return Step::ThroughDrop(reply(200, view(&state.uploads[idx])));
    }
    let id = format!(
        "upl_01K74Z3QJ8V5N2W9RTX6YB4{}{:02}",
        state.era,
        state.by_key.len()
    );
    let idx = state.uploads.len();
    state.by_key.insert(key, idx);
    state.ids.insert(id.clone(), idx);
    state.uploads.push(Upload {
        id,
        request: text,
        total: parsed
            .get("total_size")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        sha256: parsed
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        bytes: Vec::new(),
        chunks: std::collections::HashMap::new(),
        status: "receiving".into(),
        expires_at: LATER.into(),
    });
    Step::ThroughDrop(reply(201, view(&state.uploads[idx])))
}

fn mismatch(len: u64) -> Step {
    Step::ThroughDrop(reply(
        409,
        json!({"error": {
            "code": "upload_offset_mismatch",
            "message": "x",
            "details": {"retryable": false, "received_offset": len}
        }}),
    ))
}

fn do_chunk(state: &mut FakeState, request: &Request) -> Step {
    let Some(idx) = chunk_upload_id(&request.path).and_then(|id| state.ids.get(id).copied()) else {
        return Step::Now(err_reply(404, "not_found", "no such upload"));
    };
    if state.uploads[idx].status != "imported" && is_past(&state.uploads[idx].expires_at) {
        state.uploads[idx].status = "expired".into();
        return Step::Now(err_reply(404, "not_found", "the upload has expired"));
    }
    let len = state.uploads[idx].bytes.len() as u64;
    let Some(at) = header(&request.headers, "Upload-Offset")
        .parse::<u64>()
        .ok()
    else {
        return mismatch(len);
    };
    let claimed = header(&request.headers, "Chunk-Sha256").to_owned();
    if at < len && state.uploads[idx].chunks.get(&at).map(String::as_str) == Some(claimed.as_str())
    {
        return Step::ThroughDrop(reply(200, view(&state.uploads[idx])));
    }
    if at != len {
        return mismatch(len);
    }
    state.uploads[idx].chunks.insert(at, claimed);
    let body = request.body.clone().unwrap_or_default();
    state.uploads[idx].bytes.extend_from_slice(&body);
    Step::ThroughDrop(reply(200, view(&state.uploads[idx])))
}

fn do_import(state: &mut FakeState, request: &Request) -> Step {
    let raw = request.body.clone().unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap();
    let up_id = parsed
        .get("upload_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if let Some(job) = state.jobs.get(&up_id) {
        return Step::ThroughDrop(reply(202, job_json(job)));
    }
    let Some(&idx) = state.ids.get(&up_id) else {
        return Step::Now(err_reply(404, "not_found", "no such upload"));
    };
    if is_past(&state.uploads[idx].expires_at) {
        return Step::Now(err_reply(404, "not_found", "the upload has expired"));
    }
    if sha(&state.uploads[idx].bytes) != state.uploads[idx].sha256 {
        return Step::Now(err_reply(422, "upload_checksum_mismatch", "x"));
    }
    state.uploads[idx].status = "complete".into();
    if !state.jobs.contains_key(&up_id) {
        let job_id = format!("job_01K75A0B1C2D3E4F5G6H7J8K{:02}", state.jobs.len());
        state.jobs.insert(
            up_id.clone(),
            Job {
                job_id,
                kind: "import".into(),
                status: "queued".into(),
            },
        );
    }
    let job = state.jobs.get(&up_id).unwrap();
    Step::ThroughDrop(reply(202, job_json(job)))
}

type DropFn = Box<dyn FnMut(&str, usize) -> Option<Reply> + Send>;

struct FakeOs {
    state: Mutex<FakeState>,
    drop_reply: Mutex<DropFn>,
}

impl FakeOs {
    fn new<F>(era: char, drop: F) -> Self
    where
        F: FnMut(&str, usize) -> Option<Reply> + Send + 'static,
    {
        Self {
            state: Mutex::new(FakeState {
                era,
                by_key: std::collections::HashMap::new(),
                ids: std::collections::HashMap::new(),
                uploads: Vec::new(),
                jobs: std::collections::HashMap::new(),
                calls: Vec::new(),
                counts: std::collections::HashMap::new(),
            }),
            drop_reply: Mutex::new(Box::new(drop)),
        }
    }

    fn plain() -> Self {
        Self::new('M', |_, _| None)
    }

    fn handle(&self, request: Request) -> Reply {
        let kind = kind_of(&request.path);
        let (n, step) = {
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call {
                kind: kind.to_owned(),
                headers: request.headers.clone(),
                body: request.body.clone().unwrap_or_default(),
            });
            let n = {
                let count = state.counts.entry(kind.to_owned()).or_insert(0);
                *count += 1;
                *count
            };
            let step = if kind == "create" {
                do_create(&mut state, &request)
            } else if kind == "chunk" {
                do_chunk(&mut state, &request)
            } else {
                do_import(&mut state, &request)
            };
            (n, step)
        };
        match step {
            Step::Now(reply) => reply,
            Step::ThroughDrop(answer) => {
                let mut drop_reply = self.drop_reply.lock().unwrap();
                drop_reply(kind, n).unwrap_or(answer)
            }
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    fn chunks(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.kind == "chunk")
            .collect()
    }

    fn upload_count(&self) -> usize {
        self.state.lock().unwrap().uploads.len()
    }

    fn job_count(&self) -> usize {
        self.state.lock().unwrap().jobs.len()
    }

    fn snaps(&self) -> Vec<Snap> {
        self.state
            .lock()
            .unwrap()
            .uploads
            .iter()
            .map(|up| Snap {
                status: up.status.clone(),
                bytes: up.bytes.clone(),
            })
            .collect()
    }

    fn expire_all(&self) {
        for up in &mut self.state.lock().unwrap().uploads {
            up.expires_at = PAST.into();
        }
    }

    fn mark_first_expired(&self) {
        let mut state = self.state.lock().unwrap();
        state.uploads[0].status = "expired".into();
        state.uploads[0].expires_at = PAST.into();
    }
}

impl Transport for FakeOs {
    async fn send(&self, request: Request) -> Reply {
        self.handle(request)
    }
}

struct FnSend<F> {
    f: Mutex<F>,
}

impl<F> FnSend<F> {
    fn new(f: F) -> Self {
        Self { f: Mutex::new(f) }
    }
}

impl<F> Transport for FnSend<F>
where
    F: FnMut(Request) -> Reply + Send,
{
    async fn send(&self, request: Request) -> Reply {
        (self.f.lock().unwrap())(request)
    }
}

struct Shrink<'a> {
    os: &'a FakeOs,
    path: &'a Path,
}

impl Transport for Shrink<'_> {
    async fn send(&self, request: Request) -> Reply {
        let chunk = request.path.ends_with("/chunks");
        let reply = self.os.handle(request);
        if chunk {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(self.path)
                .await
                .unwrap();
            file.set_len(10).await.unwrap();
        }
        reply
    }
}

struct CutChunks {
    os: FakeOs,
    cut: AtomicBool,
}

impl Transport for CutChunks {
    async fn send(&self, request: Request) -> Reply {
        let skip = self.cut.load(Ordering::SeqCst)
            && request.path.ends_with("/chunks")
            && header(&request.headers, "Upload-Offset") != "0";
        if skip {
            local_timeout()
        } else {
            self.os.handle(request)
        }
    }
}

struct SwapOn<F> {
    os: Mutex<FakeOs>,
    reset: AtomicBool,
    when: F,
}

impl<F> SwapOn<F>
where
    F: Fn(&Request) -> bool + Send + Sync,
{
    fn new(os: FakeOs, when: F) -> Self {
        Self {
            os: Mutex::new(os),
            reset: AtomicBool::new(false),
            when,
        }
    }

    fn tripped(&self) -> bool {
        self.reset.load(Ordering::SeqCst)
    }

    fn job_count(&self) -> usize {
        self.os.lock().unwrap().job_count()
    }
}

impl<F> Transport for SwapOn<F>
where
    F: Fn(&Request) -> bool + Send + Sync,
{
    async fn send(&self, request: Request) -> Reply {
        let trip = !self.reset.load(Ordering::SeqCst) && (self.when)(&request);
        if trip {
            self.reset.store(true, Ordering::SeqCst);
            *self.os.lock().unwrap() = FakeOs::new('N', |_, _| None);
        }
        self.os.lock().unwrap().handle(request)
    }
}

struct WorkDir {
    dir: tempfile::TempDir,
}

impl WorkDir {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    async fn file(&self, size: usize, name: &str) -> (PathBuf, Vec<u8>) {
        let file_no = FILE_NO.fetch_add(1, Ordering::Relaxed) + 1;
        let mut bytes = vec![0u8; size];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = ((i * 31 + file_no as usize * 7) % 251) as u8;
        }
        let path = self.dir.path().join(name);
        tokio::fs::write(&path, &bytes).await.unwrap();
        (path, bytes)
    }
}

async fn run(path: &Path, transport: &(impl Transport + Sync)) -> Response {
    let mut nop = |_: Progress| {};
    import_file(path, transport, &mut nop).await
}

fn ok_data(res: &Response) -> &Value {
    match res {
        Response::Ok { data, .. } => data,
        Response::Err { .. } => panic!("expected ok, got {}", res.to_json()),
    }
}

fn body_sha(request: &Request) -> Value {
    serde_json::from_slice::<Value>(request.body.as_deref().unwrap_or(&[]))
        .ok()
        .and_then(|v| v.get("sha256").cloned())
        .unwrap_or_else(|| Value::String(String::new()))
}

fn good_upload() -> Value {
    json!({
        "upload_id": "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD",
        "total_size": 10,
        "received": 0,
        "status": "receiving",
        "expires_at": LATER,
    })
}

fn desktop_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("desktop-") else {
        return false;
    };
    rest.len() == 64 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[tokio::test]
async fn declares_the_file_sends_chunks_then_starts_the_import() {
    // it('declares the file, sends it in chunks of at most 768 KiB, then starts the import')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, bytes) = dir
        .file(2 * CHUNK_BYTES as usize + 10, "通告 2026.pdf")
        .await;
    let os = FakeOs::plain();
    let mut progress = Vec::new();
    let res = import_file(&path, &os, &mut |p| progress.push(p)).await;
    match &res {
        Response::Ok { status: 202, data } => {
            assert_eq!(data.get("kind").and_then(Value::as_str), Some("import"));
        }
        other => panic!("{}", other.to_json()),
    }
    assert_eq!(os.snaps()[0].bytes, bytes);
    let calls = os.calls();
    let body = serde_json::from_slice::<Value>(&calls[0].body).unwrap();
    assert_eq!(
        body,
        json!({"total_size": bytes.len(), "sha256": sha(&bytes), "filename": "通告 2026.pdf"})
    );
    assert!(desktop_key(header(&calls[0].headers, "Idempotency-Key")));
    let chunks = os.chunks();
    assert_eq!(
        chunks
            .iter()
            .map(|c| c.body.len() as u64)
            .collect::<Vec<_>>(),
        vec![CHUNK_BYTES, CHUNK_BYTES, 10]
    );
    assert_eq!(
        chunks
            .iter()
            .map(|c| header(&c.headers, "Upload-Offset").to_owned())
            .collect::<Vec<_>>(),
        vec![
            "0".to_owned(),
            CHUNK_BYTES.to_string(),
            (2 * CHUNK_BYTES).to_string()
        ]
    );
    for chunk in &chunks {
        assert_eq!(header(&chunk.headers, "Chunk-Sha256"), sha(&chunk.body));
    }
    assert_eq!(
        progress.last().copied(),
        Some(Progress {
            sent: bytes.len() as u64,
            total: bytes.len() as u64
        })
    );
}

#[tokio::test]
async fn a_lost_chunk_reply_is_replayed_and_a_kernel_timeout_is_retried() {
    // it('a chunk whose reply was lost is replayed as 200, and a kernel timeout is retried')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, bytes) = dir.file(CHUNK_BYTES as usize + 5, "通告 2026.pdf").await;
    let os = FakeOs::new('M', |kind, n| {
        if kind == "chunk" && n == 1 {
            Some(local_timeout())
        } else if kind == "chunk" && n == 3 {
            Some(kernel_timeout())
        } else {
            None
        }
    });
    assert!(run(&path, &os).await.is_ok());
    assert_eq!(os.snaps()[0].bytes, bytes);
    assert_eq!(os.job_count(), 1);
}

#[tokio::test]
async fn a_lost_import_is_continued_by_importing_again() {
    // it('an import whose replies were all lost is continued, not repeated, by importing the file again')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let lose = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&lose);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "import" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    let first = run(&path, &os).await;
    assert_eq!(first.code(), Some("backend_timeout"));
    lose.store(false, Ordering::SeqCst);
    assert!(run(&path, &os).await.is_ok());
    assert_eq!((os.upload_count(), os.job_count()), (1, 1));
}

#[tokio::test]
async fn an_upload_cut_off_midway_resumes() {
    // it('an upload cut off midway resumes from what the OS already holds')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir
        .file(2 * CHUNK_BYTES as usize + 1, "通告 2026.pdf")
        .await;
    let cut = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&cut);
    let os = FakeOs::new('M', move |kind, n| {
        (kind == "chunk" && n >= 2 && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    assert!(!run(&path, &os).await.is_ok());
    cut.store(false, Ordering::SeqCst);
    let before = os.chunks().len();
    assert!(run(&path, &os).await.is_ok());
    assert_ne!(header(&os.chunks()[before].headers, "Upload-Offset"), "0");
    assert_eq!(os.upload_count(), 1);
}

#[tokio::test]
async fn a_file_that_shrinks_stops_with_file_changed() {
    // it('a file that shrinks while it is sent stops with file_changed, never loops')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(2 * CHUNK_BYTES as usize, "通告 2026.pdf").await;
    let os = FakeOs::plain();
    let res = run(
        &path,
        &Shrink {
            os: &os,
            path: &path,
        },
    )
    .await;
    assert_eq!(res.code(), Some("file_changed"));
    assert!(os.chunks().len() <= 2);
}

#[tokio::test]
async fn refuses_replies_that_do_not_fit_the_file() {
    // it('refuses replies that do not fit the file, and stops resuming after a bound')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let good = good_upload();
    let cases = [
        ("an id that is not one", {
            let mut v = good.clone();
            v["upload_id"] = json!("../x");
            v
        }),
        ("received past the size", {
            let mut v = good.clone();
            v["received"] = json!(11);
            v
        }),
        ("another file", {
            let mut v = good.clone();
            v["total_size"] = json!(9);
            v
        }),
        ("received not an integer", {
            let mut v = good.clone();
            v["received"] = json!(1.5);
            v
        }),
    ];
    for (name, create) in cases {
        let sent = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&sent);
        let transport = FnSend::new(move |req: Request| {
            count.fetch_add(1, Ordering::SeqCst);
            if req.path.ends_with("/uploads") {
                let mut body = create.clone();
                body["sha256"] = body_sha(&req);
                reply(201, body)
            } else {
                reply(200, json!({}))
            }
        });
        let res = run(&path, &transport).await;
        assert_eq!(res.code(), Some("invalid_response"), "{name}");
        assert_eq!(sent.load(Ordering::SeqCst), 1, "{name}");
    }
    let other_sha = sha(b"other");
    let other = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = json!(other_sha);
            reply(201, body)
        } else {
            reply(200, json!({"received": 10}))
        }
    });
    assert_eq!(run(&path, &other).await.code(), Some("invalid_response"));
    let stuck = FnSend::new(|req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            reply(201, body)
        } else {
            reply(200, json!({"received": 0}))
        }
    });
    assert_eq!(run(&path, &stuck).await.code(), Some("invalid_response"));
    let flip = Arc::new(AtomicU64::new(0));
    let flips = Arc::clone(&flip);
    let cycling = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            reply(201, body)
        } else {
            let n = flips.fetch_add(1, Ordering::SeqCst);
            let at = ((n + 1) % 2) * 5;
            reply(
                409,
                json!({"error": {
                    "code": "upload_offset_mismatch",
                    "message": "x",
                    "details": {"received_offset": at}
                }}),
            )
        }
    });
    let cycling = run(&path, &cycling).await;
    assert!(!cycling.is_ok());
    assert_eq!(cycling.status(), 409);
    assert!(flip.load(Ordering::SeqCst) <= 10);
}

#[tokio::test]
async fn a_file_it_cannot_read_is_file_unreadable() {
    // it('a file it cannot read is file_unreadable, without its path')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "secret-name.pdf").await;
    tokio::fs::remove_file(&path).await.unwrap();
    let res = run(&path, &FakeOs::plain()).await;
    assert_eq!(res.code(), Some("file_unreadable"));
    assert!(!res.to_json().to_string().contains("secret-name"));
}

#[tokio::test]
async fn a_forbidden_name_is_not_sent_and_an_os_error_is_returned() {
    // it('a name the contract forbids is not sent; an OS error is returned as it is')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "report\\draft.pdf").await;
    let os = FakeOs::plain();
    assert!(run(&path, &os).await.is_ok());
    let body = serde_json::from_slice::<Value>(&os.calls()[0].body).unwrap();
    assert!(body.get("filename").is_none());
    let (empty, _) = dir.file(0, "empty.pdf").await;
    let res = run(&empty, &FakeOs::plain()).await;
    assert_eq!(res.status(), 400);
    assert_eq!(res.code(), Some("invalid_request"));
}

#[tokio::test]
async fn a_retry_a_second_import_or_two_at_once_reach_one_job() {
    // it('a retry, a second import or two at once reach one upload and one job')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(CHUNK_BYTES as usize + 3, "通告 2026.pdf").await;
    let lose = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&lose);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "import" && flag.load(Ordering::SeqCst)).then(|| {
            reply(
                502,
                json!({"error": {"code": "upstream_unavailable", "message": "x"}}),
            )
        })
    });
    assert_eq!(run(&path, &os).await.code(), Some("upstream_unavailable"));
    lose.store(false, Ordering::SeqCst);
    let (a, b) = tokio::join!(run(&path, &os), run(&path, &os));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!((os.upload_count(), os.job_count()), (1, 1));
    assert_eq!(ok_data(&a).get("job_id"), ok_data(&b).get("job_id"));
}

#[tokio::test]
async fn a_renamed_copy_is_a_new_import() {
    // it('a renamed copy is a new import, never a reused key with another request')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, bytes) = dir.file(10, "a.pdf").await;
    let os = FakeOs::plain();
    assert!(run(&path, &os).await.is_ok());
    let copy = dir.path().join("b.pdf");
    tokio::fs::write(&copy, &bytes).await.unwrap();
    assert!(run(&copy, &os).await.is_ok());
    assert_eq!((os.upload_count(), os.job_count()), (2, 2));
}

#[tokio::test]
async fn an_upload_that_expired_before_its_import_is_replaced() {
    // it('an upload that expired before its import is replaced by a new one')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let cut = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&cut);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "chunk" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    assert!(!run(&path, &os).await.is_ok());
    os.expire_all();
    cut.store(false, Ordering::SeqCst);
    assert!(run(&path, &os).await.is_ok());
    assert_eq!(os.upload_count(), 2);
    let mut keys = std::collections::HashSet::new();
    for call in os.calls() {
        if call.kind == "create" {
            keys.insert(header(&call.headers, "Idempotency-Key").to_owned());
        }
    }
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn an_unknown_or_undispatched_reply_is_sent_again() {
    // it('a reply whose outcome is unknown, or that was never dispatched, is sent again (ADR-DOC-02 §6)')
    let _guard = begin().await;
    let dir = WorkDir::new();
    for code in [
        "upstream_unavailable",
        "module_panicked",
        "module_killed",
        "stop_failed",
        "duplicate_request_id",
    ] {
        let (path, _) = dir.file(10, "通告 2026.pdf").await;
        let code_owned = code.to_owned();
        let os = FakeOs::new('M', move |kind, n| {
            (kind == "import" && n == 1)
                .then(|| reply(502, json!({"error": {"code": code_owned, "message": "x"}})))
        });
        assert!(run(&path, &os).await.is_ok(), "{code}");
        let imports = os.calls().iter().filter(|c| c.kind == "import").count();
        assert_eq!(imports, 2, "{code}");
        assert_eq!(os.job_count(), 1, "{code}");
    }
}

#[tokio::test]
async fn an_import_that_started_before_expiry_is_found_again() {
    // it('an import that started before its upload expired is found again, never repeated')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let lose = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&lose);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "import" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    assert!(!run(&path, &os).await.is_ok());
    os.expire_all();
    lose.store(false, Ordering::SeqCst);
    assert!(run(&path, &os).await.is_ok());
    assert_eq!((os.upload_count(), os.job_count()), (1, 1));
}

#[tokio::test]
async fn more_expired_uploads_than_a_small_bound_still_import() {
    // it('more expired uploads than any small bound still lead to an import')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let cut = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&cut);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "chunk" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    for _ in 0..6 {
        assert!(!run(&path, &os).await.is_ok());
        os.expire_all();
    }
    cut.store(false, Ordering::SeqCst);
    assert!(run(&path, &os).await.is_ok());
    assert_eq!(os.job_count(), 1);
    assert!(os.upload_count() > 5);
}

#[tokio::test]
async fn an_upload_that_expires_midway_is_replaced_in_the_same_run() {
    // it('an upload that expires midway is replaced in the same run, from its chunk request 404')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, bytes) = dir.file(CHUNK_BYTES as usize + 7, "通告 2026.pdf").await;
    let transport = CutChunks {
        os: FakeOs::plain(),
        cut: AtomicBool::new(true),
    };
    assert!(!run(&path, &transport).await.is_ok());
    transport.os.expire_all();
    transport.cut.store(false, Ordering::SeqCst);
    assert!(run(&path, &transport).await.is_ok());
    let snaps = transport.os.snaps();
    assert_eq!(snaps[0].status, "expired");
    assert_eq!(snaps[1].bytes, bytes);
}

#[tokio::test]
async fn past_64_expired_uploads_a_run_reaches_a_live_one() {
    // it('past 64 expired uploads a run still reaches a live one, starting where the last run stopped')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let cut = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&cut);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "chunk" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    for _ in 0..70 {
        assert!(!run(&path, &os).await.is_ok());
        os.expire_all();
    }
    cut.store(false, Ordering::SeqCst);
    let creates = os.calls().iter().filter(|c| c.kind == "create").count();
    assert!(run(&path, &os).await.is_ok());
    assert!(os.upload_count() > 64);
    assert_eq!(os.job_count(), 1);
    let later = os.calls().iter().filter(|c| c.kind == "create").count();
    assert!(later - creates < 8);
}

#[tokio::test]
async fn an_expired_upload_whose_import_started_answers_with_that_job() {
    // it('an expired upload whose import started answers with that job, not a new upload')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let os = FakeOs::plain();
    let first = run(&path, &os).await;
    os.mark_first_expired();
    let again = run(&path, &os).await;
    assert_eq!(ok_data(&again), ok_data(&first));
    assert_eq!((os.upload_count(), os.job_count()), (1, 1));
}

#[tokio::test]
async fn after_the_os_loses_its_data_every_run_takes_the_same_upload() {
    // it('after the OS loses its data, the remembered place is dropped and every run takes the same upload')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let old = FakeOs::new('M', |kind, _| (kind == "chunk").then(local_timeout));
    for _ in 0..3 {
        assert!(!run(&path, &old).await.is_ok());
        old.expire_all();
    }
    let os = FakeOs::new('N', |_, _| None);
    let first = run(&path, &os).await;
    forget_chains();
    let again = run(&path, &os).await;
    assert_eq!(ok_data(&again), ok_data(&first));
    assert_eq!(os.job_count(), 1);
}

#[tokio::test]
async fn losing_data_between_create_and_chunks_does_not_fork() {
    // it('the OS losing its data between creating an upload and its chunks does not fork the chain')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let send = SwapOn::new(FakeOs::plain(), |req: &Request| {
        req.path.ends_with("/chunks")
    });
    let first = run(&path, &send).await;
    let again = run(&path, &send).await;
    assert_eq!(ok_data(&again), ok_data(&first));
    assert_eq!(send.job_count(), 1);
}

#[tokio::test]
async fn losing_data_just_before_the_next_upload_does_not_fork() {
    // it('the OS losing its data just before the next upload in the chain does not fork it')
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "通告 2026.pdf").await;
    let cut = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&cut);
    let os = FakeOs::new('M', move |kind, _| {
        (kind == "chunk" && flag.load(Ordering::SeqCst)).then(local_timeout)
    });
    assert!(!run(&path, &os).await.is_ok());
    os.expire_all();
    cut.store(false, Ordering::SeqCst);
    let key0 = header(&os.calls()[0].headers, "Idempotency-Key").to_owned();
    let send = SwapOn::new(os, move |req: &Request| {
        req.path.ends_with("/uploads") && header(&req.headers, "Idempotency-Key") != key0
    });
    let first = run(&path, &send).await;
    assert!(send.tripped());
    forget_chains();
    let again = run(&path, &send).await;
    assert_eq!(ok_data(&again), ok_data(&first));
    assert_eq!(send.job_count(), 1);
}

/// The error envelope reaches the caller whole, `hint` included (ERR-1), as
/// the desktop's `toResponse` passed it on; a reply without one is
/// `invalid_response`.
#[test]
fn the_error_envelope_keeps_its_hint_and_details() {
    use crate::response::to_response;
    use crate::transport::Reply;
    let body = br#"{"error":{"code":"conflict","message":"m","hint":"try again","details":{"received_offset":3}}}"#;
    let res = to_response(Reply::Http {
        status: 409,
        body: body.to_vec(),
    });
    let Response::Err { status, error } = res else {
        panic!("not an error: {res:?}")
    };
    assert_eq!(status, 409);
    assert_eq!(error.hint.as_deref(), Some("try again"));
    assert_eq!(
        error.details,
        Some(serde_json::json!({"received_offset": 3}))
    );
    let bare = to_response(Reply::Http {
        status: 500,
        body: b"oops".to_vec(),
    });
    assert_eq!(bare.code(), Some("invalid_response"));
    let null = to_response(Reply::Http {
        status: 200,
        body: b"null".to_vec(),
    });
    assert_eq!(null.code(), Some("invalid_response"));
}

/// A 409 `upload_offset_mismatch` (a chunk that landed before its reply was
/// lost) carries on from the OS's `received_offset`, not from the start.
#[tokio::test]
async fn an_offset_mismatch_carries_on_from_what_the_os_holds() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let size = CHUNK_BYTES + 5;
    let (path, _) = dir.file(size as usize, "a.pdf").await;
    let offsets = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&offsets);
    let os = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            body["total_size"] = json!(size);
            reply(201, body)
        } else if req.path.ends_with("/chunks") {
            let at = header(&req.headers, "Upload-Offset").to_owned();
            seen.lock().unwrap().push(at.clone());
            if at == "0" {
                reply(
                    409,
                    json!({"error": {"code": "upload_offset_mismatch", "message": "x",
                        "details": {"received_offset": CHUNK_BYTES}}}),
                )
            } else {
                reply(200, json!({"received": size}))
            }
        } else {
            reply(202, json!({"job_id": "job_01K74Z3QJ8V5N2W9RTX6YB4MCD"}))
        }
    });
    assert!(run(&path, &os).await.is_ok());
    assert_eq!(
        *offsets.lock().unwrap(),
        vec!["0".to_owned(), CHUNK_BYTES.to_string()]
    );
}

/// An upload the OS answers as `expired` gets no chunks: the run goes
/// straight to its import, then to the next upload in the chain.
#[tokio::test]
async fn an_expired_upload_gets_no_chunks() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    const OLD: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    const NEW: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCE";
    let chunked = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&chunked);
    let first = Arc::new(Mutex::new(None::<String>));
    let os = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let key = header(&req.headers, "Idempotency-Key").to_owned();
            let mut root = first.lock().unwrap();
            let old = root.get_or_insert_with(|| key.clone()) == &key;
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            body["upload_id"] = json!(if old { OLD } else { NEW });
            body["status"] = json!(if old { "expired" } else { "receiving" });
            reply(if old { 200 } else { 201 }, body)
        } else if req.path.ends_with("/chunks") {
            seen.lock().unwrap().push(req.path.clone());
            reply(200, json!({"received": 10}))
        } else {
            let body = serde_json::from_slice::<Value>(req.body.as_deref().unwrap_or(&[])).unwrap();
            if body["upload_id"] == OLD {
                reply(404, json!({"error": {"code": "not_found", "message": "x"}}))
            } else {
                reply(202, json!({"job_id": "job_01K74Z3QJ8V5N2W9RTX6YB4MCD"}))
            }
        }
    });
    assert!(run(&path, &os).await.is_ok());
    let chunked = chunked.lock().unwrap();
    assert!(
        !chunked.is_empty() && chunked.iter().all(|p| p.contains(NEW)),
        "{chunked:?}"
    );
}

/// A valid upload id per number, for scripted OSes that hand out many.
fn upl(n: usize) -> String {
    const B32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut tail = String::new();
    let mut v = n;
    for _ in 0..5 {
        tail.insert(0, B32[v % 32] as char);
        v /= 32;
    }
    format!("upl_01K74Z3QJ8V5N2W9RTX6Y{tail}")
}

/// Each request goes out with its method and content type: the OS parses
/// the JSON bodies only under `application/json`.
#[tokio::test]
async fn every_request_has_its_method_and_content_type() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    let seen = Arc::new(Mutex::new(Vec::<(Method, String, String)>::new()));
    let log = Arc::clone(&seen);
    let os = FnSend::new(move |req: Request| {
        let kind = req.path.rsplit('/').next().unwrap_or("").to_owned();
        let ctype = header(&req.headers, "Content-Type").to_owned();
        log.lock().unwrap().push((req.method, kind.clone(), ctype));
        match kind.as_str() {
            "uploads" => {
                let mut body = good_upload();
                body["sha256"] = body_sha(&req);
                reply(201, body)
            }
            "chunks" => reply(200, json!({"received": 10})),
            _ => reply(202, json!({"job_id": "job_01K74Z3QJ8V5N2W9RTX6YB4MCD"})),
        }
    });
    assert!(run(&path, &os).await.is_ok());
    let json_type = "application/json".to_owned();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            (Method::Post, "uploads".to_owned(), json_type.clone()),
            (
                Method::Post,
                "chunks".to_owned(),
                "application/octet-stream".to_owned()
            ),
            (Method::Post, "imports".to_owned(), json_type),
        ]
    );
}

/// The codes sent again, as `documents-import.ts` lists them (ADR-DOC-02
/// §6): written out here, not read from `RETRY`, so a code dropped or added
/// there turns this red.
const RETRY_CONTRACT: [&str; 16] = [
    "backend_not_ready",
    "backend_unreachable",
    "backend_timeout",
    "module_overloaded",
    "module_stopping",
    "module_not_ready",
    "module_draining",
    "request_abandoned",
    "upstream_timeout",
    "upstream_connection_closed",
    "upstream_unavailable",
    "upstream_response_too_large",
    "duplicate_request_id",
    "module_panicked",
    "module_killed",
    "stop_failed",
];

/// Every code in the contract is sent again, at most three sends per step;
/// any other error is returned after one send.
#[tokio::test]
async fn each_retry_code_is_sent_again_up_to_three_times() {
    let _guard = begin().await;
    let mut ours = RETRY.to_vec();
    let mut want = RETRY_CONTRACT.to_vec();
    ours.sort_unstable();
    want.sort_unstable();
    assert_eq!(ours, want);
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    for fails in [2u64, 3] {
        for code in RETRY_CONTRACT.iter().copied().chain(["conflict"]) {
            let sends = Arc::new(AtomicU64::new(0));
            let n = Arc::clone(&sends);
            let os = FnSend::new(move |req: Request| {
                if req.path.ends_with("/uploads") {
                    if n.fetch_add(1, Ordering::SeqCst) < fails {
                        return reply(503, json!({"error": {"code": code, "message": "x"}}));
                    }
                    let mut body = good_upload();
                    body["sha256"] = body_sha(&req);
                    reply(201, body)
                } else if req.path.ends_with("/chunks") {
                    reply(200, json!({"received": 10}))
                } else {
                    reply(202, json!({"job_id": "job_01K74Z3QJ8V5N2W9RTX6YB4MCD"}))
                }
            });
            let res = run(&path, &os).await;
            let creates = sends.load(Ordering::SeqCst);
            if code == "conflict" {
                assert_eq!((res.code(), creates), (Some("conflict"), 1), "{code}");
            } else if fails == 2 {
                assert!(
                    res.is_ok() && creates == 3,
                    "{code}: {creates} sends, {res:?}"
                );
            } else {
                assert_eq!((res.code(), creates), (Some(code), 3), "{code}");
            }
        }
    }
}

/// An OS that keeps moving the offset gets 8 resumes, then its reply: 9
/// chunk sends in all.
#[tokio::test]
async fn resumes_stop_after_eight() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    let chunks = Arc::new(AtomicU64::new(0));
    let n = Arc::clone(&chunks);
    let os = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            return reply(201, body);
        }
        let at = if n.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
            5
        } else {
            0
        };
        reply(
            409,
            json!({"error": {"code": "upload_offset_mismatch", "message": "x",
                "details": {"received_offset": at}}}),
        )
    });
    assert_eq!(run(&path, &os).await.code(), Some("upload_offset_mismatch"));
    assert_eq!(chunks.load(Ordering::SeqCst), 9);
}

/// One run walks at most 64 expired uploads, then says so.
#[tokio::test]
async fn one_run_walks_at_most_64_expired_uploads() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    let ids = Arc::new(Mutex::new(
        std::collections::HashMap::<String, String>::new(),
    ));
    let imports = Arc::new(AtomicU64::new(0));
    let n = Arc::clone(&imports);
    let os = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let key = header(&req.headers, "Idempotency-Key").to_owned();
            let mut ids = ids.lock().unwrap();
            let next = upl(ids.len());
            let id = ids.entry(key).or_insert(next).clone();
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            body["upload_id"] = json!(id);
            body["status"] = json!("expired");
            reply(200, body)
        } else if req.path.ends_with("/imports") {
            n.fetch_add(1, Ordering::SeqCst);
            reply(404, json!({"error": {"code": "not_found", "message": "x"}}))
        } else {
            reply(
                500,
                json!({"error": {"code": "internal", "message": "no chunks expected"}}),
            )
        }
    });
    let res = run(&path, &os).await;
    assert_eq!(
        res,
        Response::fail(404, "not_found", "the upload keeps expiring")
    );
    assert_eq!(imports.load(Ordering::SeqCst), 64);
}

/// The chain memory keeps the newest 1024 files; remembering a file again
/// makes it the newest.
#[tokio::test]
async fn the_chain_memory_keeps_the_newest_1024_files() {
    let _guard = begin().await;
    // 1024 written out, not `MAX_TAILS`, so the bound itself is pinned.
    for i in 0..=1024 {
        chain_remember(&format!("k{i}"), "r", "a");
    }
    assert_eq!(chain_hint("k0"), None);
    assert_eq!(chain_hint("k1"), Some(("r".to_owned(), "a".to_owned())));
    assert!(chain_hint("k1024").is_some());
    assert_eq!(with_tails(|t| t.len()), 1024);
    chain_remember("k1", "r", "b");
    chain_remember("extra", "r", "a");
    assert_eq!(chain_hint("k2"), None);
    assert_eq!(chain_hint("k1"), Some(("r".to_owned(), "b".to_owned())));
    forget_chains();
}

/// A file that cannot be read, wherever that is found out, answers only
/// that: never the path, never the I/O error's text.
#[tokio::test]
async fn an_unreadable_file_says_only_that() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let want = Response::fail(400, "file_unreadable", "the file could not be read");
    let missing = dir.path().join("gone.pdf");
    assert_eq!(run(&missing, &FakeOs::plain()).await, want);
    assert_eq!(run(dir.path(), &FakeOs::plain()).await, want);
    let mut nop = |_: Progress| {};
    let reopened = upload(&missing, 10, HELLO_SHA, &FakeOs::plain(), &mut nop).await;
    assert_eq!(reopened, want);
}

/// A chunk reply that claims more than the file holds is refused, and no
/// import starts on it.
#[tokio::test]
async fn a_chunk_reply_past_the_size_is_refused() {
    let _guard = begin().await;
    let dir = WorkDir::new();
    let (path, _) = dir.file(10, "a.pdf").await;
    let imports = Arc::new(AtomicU64::new(0));
    let n = Arc::clone(&imports);
    let os = FnSend::new(move |req: Request| {
        if req.path.ends_with("/uploads") {
            let mut body = good_upload();
            body["sha256"] = body_sha(&req);
            reply(201, body)
        } else if req.path.ends_with("/chunks") {
            reply(200, json!({"received": 11}))
        } else {
            n.fetch_add(1, Ordering::SeqCst);
            reply(202, json!({"job_id": "job_01K74Z3QJ8V5N2W9RTX6YB4MCD"}))
        }
    });
    let res = run(&path, &os).await;
    assert_eq!(
        res,
        Response::fail(502, "invalid_response", "the upload did not move forward")
    );
    assert_eq!(imports.load(Ordering::SeqCst), 0);
}
