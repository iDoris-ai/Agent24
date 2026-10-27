#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use serde_json::json;

use super::*;
use crate::state::AppState;
use crate::text_layer::{Block, Line, ParseStatus};

/// An engine that reads every file as one block of its bytes, counting its
/// parses and, while `hold` is set, waiting for `go` before answering.
struct Fake {
    parses: AtomicUsize,
    running: Arc<AtomicUsize>,
    most: AtomicUsize,
    hold: std::sync::atomic::AtomicBool,
    panic: std::sync::atomic::AtomicBool,
    /// Permits to finish, while `hold` is set; one kept for a late parse.
    go: Arc<tokio::sync::Semaphore>,
    answer: Mutex<Option<Result<TextLayer, EngineError>>>,
    /// Run as a parse ends, in the same poll that answers.
    then: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// What the fake reads from a file holding `text`.
fn layer_of(text: &str) -> TextLayer {
    TextLayer {
        v: 1,
        content_sha256: sha(text.as_bytes()),
        pages: 1,
        engine: EngineRef {
            id: "fake".into(),
            version: "1".into(),
        },
        config: json!({ "blocks": "whole" }),
        parse_status: ParseStatus::Complete,
        unparsed_regions: vec![],
        blocks: vec![Block {
            block_id: "p1/b1".into(),
            page: 1,
            text: text.into(),
            lines: vec![Line {
                start: 0,
                end: text.len(),
                rect: [0.0, 0.0, 10.0, 10.0],
            }],
        }],
    }
}

impl Engine for Fake {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "fake".into(),
            version: "1".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "blocks": "whole" })
    }
    fn parse(&self, _content_sha256: &str, _media_type: &str, path: &Path) -> Parse {
        assert!(!self.panic.load(SeqCst), "the engine crashed");
        self.parses.fetch_add(1, SeqCst);
        let now = self.running.fetch_add(1, SeqCst) + 1;
        self.most.fetch_max(now, SeqCst);
        let text = std::fs::read_to_string(path).unwrap();
        let answer = self
            .answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(layer_of(&text)));
        let (hold, go) = (self.hold.load(SeqCst), self.go.clone());
        let then = self.then.lock().unwrap().clone();
        // Counts out when the parse ends or is dropped.
        let parsing = Parsing(self.running.clone());
        Box::pin(async move {
            if hold {
                go.acquire().await.unwrap().forget();
            }
            drop(parsing);
            if let Some(then) = then {
                then();
            }
            answer
        })
    }
}

struct Parsing(Arc<AtomicUsize>);

impl Drop for Parsing {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

fn fake() -> Arc<Fake> {
    Arc::new(Fake {
        parses: AtomicUsize::default(),
        running: Arc::default(),
        most: AtomicUsize::default(),
        hold: false.into(),
        panic: false.into(),
        go: Arc::new(tokio::sync::Semaphore::new(0)),
        answer: Mutex::default(),
        then: Mutex::default(),
    })
}

struct Env {
    _dir: tempfile::TempDir,
    _state: AppState,
    storage: Arc<Storage>,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    Env {
        _dir: dir,
        _state: state,
        storage,
    }
}

fn stored(env: &Env, text: &str) -> String {
    env.storage.blobs.put_bytes(text.as_bytes()).unwrap().sha256
}

fn layers(f: &Arc<Fake>) -> Arc<Layers> {
    let engine: Arc<dyn Engine> = f.clone();
    Layers::new(Some(engine))
}

const PDF: &str = "application/pdf";

#[tokio::test]
async fn a_layer_is_built_and_pinned_once_then_read_from_the_pin() {
    let (env, f) = (env().await, fake());
    let l = layers(&f);
    let content = stored(&env, "通告");
    let first = l
        .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
        .await
        .unwrap();
    assert_eq!(
        text_layer::load(&env.storage, &first).await.unwrap(),
        layer_of("通告")
    );
    assert_eq!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await
            .unwrap(),
        first
    );
    assert_eq!(f.parses.load(SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_that_come_together_share_one_build() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    let content = stored(&env, "a");
    let reads: Vec<_> = (0..5)
        .map(|_| {
            let (l, s, c) = (l.clone(), env.storage.clone(), content.clone());
            tokio::spawn(async move { l.layer(&s, &c, PDF, WAIT, Requester::Page).await })
        })
        .collect();
    wait_until(|| f.running.load(SeqCst) == 1).await;
    f.go.add_permits(1);
    let got: Vec<_> = futures_join(reads).await;
    assert!(
        got.windows(2).all(|w| w[0] == w[1]) && got[0].is_ok(),
        "{got:?}"
    );
    assert_eq!(f.parses.load(SeqCst), 1);
}

async fn futures_join<T: Send + 'static>(tasks: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_that_waits_too_long_is_pending_and_the_build_goes_on() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    let content = stored(&env, "slow");
    // Pending, whether before or after the build was admitted; then until
    // it surely was.
    admitted(&l, &env, &content, 1, Requester::Page).await;
    f.go.add_permits(1);
    let start = std::time::Instant::now();
    let (engine, config) = (f.engine(), config_sha256(&f.config()));
    loop {
        let pinned = text_layer::pinned(&env.storage, &content, &engine, &config);
        if pinned.await.unwrap().is_some() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
    assert_eq!(f.parses.load(SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_most_two_engines_run_and_too_many_waiting_builds_are_busy() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    for i in 0..PARALLEL + QUEUE {
        let content = stored(&env, &format!("doc {i}"));
        admitted(&l, &env, &content, i + 1, Requester::Page).await;
    }
    let content = stored(&env, "one too many");
    // Refused at once, whatever the wait.
    assert_eq!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await,
        Err(LayerFailure::Busy)
    );
    wait_until(|| f.running.load(SeqCst) == PARALLEL).await;
    assert_eq!(f.most.load(SeqCst), PARALLEL);
    // Released, every build ends, never more than two at once.
    f.go.add_permits(PARALLEL + QUEUE);
    wait_until(|| f.parses.load(SeqCst) == PARALLEL + QUEUE && f.running.load(SeqCst) == 0).await;
    assert_eq!(f.most.load(SeqCst), PARALLEL);
}

#[tokio::test]
async fn an_engine_failure_is_reported_and_not_remembered() {
    let (env, f) = (env().await, fake());
    let l = layers(&f);
    let content = stored(&env, "x");
    *f.answer.lock().unwrap() = Some(Err(EngineError::Unsupported));
    let got = l
        .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
        .await;
    assert_eq!(got, Err(LayerFailure::Engine(EngineError::Unsupported)));
    // A layer that breaks §3.1, or names another engine, is a failed parse.
    let mut bad = layer_of("x");
    bad.v = 2;
    *f.answer.lock().unwrap() = Some(Ok(bad));
    let got = l
        .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
        .await;
    assert!(
        matches!(got, Err(LayerFailure::Engine(EngineError::Failed(_)))),
        "{got:?}"
    );
    let mut other = layer_of("x");
    other.engine.version = "2".into();
    *f.answer.lock().unwrap() = Some(Ok(other));
    let got = l
        .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
        .await;
    assert!(
        matches!(got, Err(LayerFailure::Engine(EngineError::Failed(_)))),
        "{got:?}"
    );
    // Each read tried again; once the engine answers, the layer is pinned.
    *f.answer.lock().unwrap() = None;
    assert!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
    assert_eq!(f.parses.load(SeqCst), 4);
}

#[tokio::test]
async fn without_an_engine_there_is_no_layer() {
    let env = env().await;
    let content = stored(&env, "x");
    let got = Layers::new(None)
        .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
        .await;
    assert_eq!(got, Err(LayerFailure::NoEngine));
}

async fn wait_until(done: impl Fn() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "never happened");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_that_panics_fails_its_build_and_leaves_no_trace() {
    let (env, f) = (env().await, fake());
    let l = layers(&f);
    f.panic.store(true, SeqCst);
    for i in 0..PARALLEL + QUEUE + 1 {
        let content = stored(&env, &format!("doc {i}"));
        let got = l
            .layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await;
        assert!(
            matches!(got, Err(LayerFailure::Engine(EngineError::Failed(_)))),
            "{got:?}"
        );
    }
    assert_eq!(l.building.lock().unwrap().len(), 0);
    f.panic.store(false, SeqCst);
    let content = stored(&env, "doc 0");
    assert!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_build_finds_a_layer_pinned_since_its_lookup_and_does_not_parse() {
    let (env, f) = (env().await, fake());
    let content = stored(&env, "x");
    let address = text_layer::pin(&env.storage, &content, &layer_of("x"))
        .await
        .unwrap();
    let engine: Arc<dyn Engine> = f.clone();
    let key = LayerKey::of(engine.as_ref(), &content);
    let never = watch::channel(false).1;
    let got = build(engine, env.storage.clone(), key, PDF.into(), never).await;
    assert_eq!(got, Ok(address));
    assert_eq!(f.parses.load(SeqCst), 0);
    // One that cannot be stopped parses and pins.
    let other = stored(&env, "y");
    let engine: Arc<dyn Engine> = f.clone();
    let key = LayerKey::of(engine.as_ref(), &other);
    let never = watch::channel(false).1;
    assert!(
        build(engine, env.storage.clone(), key, PDF.into(), never)
            .await
            .is_ok()
    );
    assert_eq!(f.parses.load(SeqCst), 1);
}

/// Asks for `content`'s layer with a short wait until its build is one of
/// `started` builds under way: the wait also covers the first lookup, so a
/// slow one may end it before the build is admitted.
async fn admitted(l: &Arc<Layers>, env: &Env, content: &str, started: usize, by: Requester) {
    let start = std::time::Instant::now();
    loop {
        let got = l
            .layer(
                &env.storage,
                content,
                PDF,
                Duration::from_millis(20),
                by.clone(),
            )
            .await;
        assert_eq!(got, Err(LayerFailure::Pending));
        if l.building.lock().unwrap().len() >= started {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never admitted");
    }
}

/// Asks for `content`'s layer `by` until `by` is one of its build's
/// requesters: a slow first lookup may end a short wait before it joins.
async fn joined(l: &Arc<Layers>, env: &Env, content: &str, by: Requester) {
    let start = std::time::Instant::now();
    loop {
        let got = l
            .layer(
                &env.storage,
                content,
                PDF,
                Duration::from_millis(20),
                by.clone(),
            )
            .await;
        assert_eq!(got, Err(LayerFailure::Pending));
        if l.building
            .lock()
            .unwrap()
            .values()
            .any(|b| b.by.contains(&by))
        {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never joined");
    }
}

fn tool(op: &str) -> Requester {
    Requester::Tool(crate::authorization::Accepted::bare(op))
}

/// Stops the builds `op`'s tool calls asked for.
fn stop_tool(l: &Layers, op: &str) -> Vec<watch::Receiver<Outcome>> {
    let op = tool(op);
    l.stop(|r| *r == op)
}

/// What a build ended with, once its task has ended.
async fn ended(mut outcome: watch::Receiver<Outcome>) -> Result<String, LayerFailure> {
    let done = tokio::time::timeout(Duration::from_secs(5), outcome.wait_for(Option::is_some));
    done.await.expect("never ended").unwrap().clone().unwrap()
}

/// K1-06a-grant-path §2.5: a build a revoked operation asked for stops:
/// its parse is dropped, nothing is pinned, and a later read builds afresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_build_drops_its_parse_and_pins_nothing() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    let content = stored(&env, "x");
    admitted(&l, &env, &content, 1, tool("find")).await;
    wait_until(|| f.running.load(SeqCst) == 1).await;
    let stopped = stop_tool(&l, "find");
    assert_eq!(stopped.len(), 1);
    assert_eq!(ended(stopped[0].clone()).await, Err(LayerFailure::Stopped));
    assert_eq!(f.running.load(SeqCst), 0, "the parse goes on");
    assert!(l.building.lock().unwrap().is_empty());
    f.hold.store(false, SeqCst);
    assert!(
        l.layer(&env.storage, &content, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
    assert_eq!(f.parses.load(SeqCst), 2);
}

/// Only the builds the stopped requester asked for stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_leaves_the_builds_others_asked_for() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    let (page, job) = (stored(&env, "page"), stored(&env, "job"));
    admitted(&l, &env, &page, 1, Requester::Page).await;
    let first = Requester::Job {
        id: "job_1".into(),
        attempt: 1,
    };
    admitted(&l, &env, &job, 2, first).await;
    wait_until(|| f.running.load(SeqCst) == 2).await;
    assert!(stop_tool(&l, "find").is_empty());
    let second = Requester::Job {
        id: "job_1".into(),
        attempt: 2,
    };
    assert!(l.stop(|r| *r == second).is_empty());
    assert_eq!(f.running.load(SeqCst), 2);
    f.go.add_permits(2);
    assert!(
        l.layer(&env.storage, &page, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
    assert!(
        l.layer(&env.storage, &job, PDF, WAIT, Requester::Page)
            .await
            .is_ok()
    );
    assert_eq!(f.parses.load(SeqCst), 2);
}

/// A build a page and a tool call both asked for stops for the tool: over
/// stopping is the safe side (§2.5); the page's read may try again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_shared_with_a_page_stops_for_the_tool_that_also_asked() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    let content = stored(&env, "shared");
    // The page starts the build and waits on it, past `joined`'s limit.
    let page = {
        let (l, s, c) = (l.clone(), env.storage.clone(), content.clone());
        let wait = Duration::from_secs(30);
        tokio::spawn(async move { l.layer(&s, &c, PDF, wait, Requester::Page).await })
    };
    wait_until(|| f.running.load(SeqCst) == 1).await;
    joined(&l, &env, &content, tool("read_range")).await;
    let stopped = stop_tool(&l, "read_range");
    assert_eq!(stopped.len(), 1);
    assert_eq!(ended(stopped[0].clone()).await, Err(LayerFailure::Stopped));
    assert_eq!(page.await.unwrap(), Err(LayerFailure::Stopped));
}

/// A build still waiting for an engine stops without parsing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_build_waiting_for_an_engine_stops_without_parsing() {
    let (env, f) = (env().await, fake());
    f.hold.store(true, SeqCst);
    let l = layers(&f);
    for i in 0..PARALLEL {
        let content = stored(&env, &format!("doc {i}"));
        admitted(&l, &env, &content, i + 1, Requester::Page).await;
    }
    wait_until(|| f.running.load(SeqCst) == PARALLEL).await;
    let waiting = stored(&env, "waiting");
    admitted(&l, &env, &waiting, PARALLEL + 1, tool("find")).await;
    let stopped = stop_tool(&l, "find");
    assert_eq!(stopped.len(), 1);
    assert_eq!(ended(stopped[0].clone()).await, Err(LayerFailure::Stopped));
    assert_eq!(f.parses.load(SeqCst), PARALLEL);
    f.go.add_permits(PARALLEL);
    wait_until(|| f.running.load(SeqCst) == 0).await;
    assert_eq!(f.parses.load(SeqCst), PARALLEL);
}

/// Stopped as its parse ends, before its pin: the last point it stops at.
#[tokio::test]
async fn a_build_stopped_as_its_parse_ends_pins_nothing() {
    let (env, f) = (env().await, fake());
    let l = layers(&f);
    let stopping = Arc::downgrade(&l);
    *f.then.lock().unwrap() = Some(Arc::new(move || {
        if let Some(l) = stopping.upgrade() {
            stop_tool(&l, "find");
        }
    }));
    let content = stored(&env, "late");
    let got = l
        .layer(&env.storage, &content, PDF, WAIT, tool("find"))
        .await;
    assert_eq!(got, Err(LayerFailure::Stopped));
    let (engine, config) = (f.engine(), config_sha256(&f.config()));
    let pinned = text_layer::pinned(&env.storage, &content, &engine, &config);
    assert_eq!(pinned.await.unwrap(), None);
    assert_eq!(f.parses.load(SeqCst), 1);
}

/// A layer pinned since the read's lookup is not given to a build told to
/// stop meanwhile.
#[tokio::test]
async fn a_stopped_build_does_not_hand_over_a_layer_pinned_since_its_lookup() {
    let (env, f) = (env().await, fake());
    let content = stored(&env, "x");
    text_layer::pin(&env.storage, &content, &layer_of("x"))
        .await
        .unwrap();
    let engine: Arc<dyn Engine> = f.clone();
    let key = LayerKey::of(engine.as_ref(), &content);
    let (stop, stopped) = watch::channel(false);
    stop.send_replace(true);
    let got = build(engine, env.storage.clone(), key, PDF.into(), stopped).await;
    assert_eq!(got, Err(LayerFailure::Stopped));
    assert_eq!(f.parses.load(SeqCst), 0);
}

/// A build told to stop whose second lookup fails ends stopped, not as
/// storage's failure (#1206 gate 3).
#[tokio::test]
async fn a_stopped_build_whose_lookup_fails_ends_stopped() {
    let (env, f) = (env().await, fake());
    let content = stored(&env, "x");
    let engine: Arc<dyn Engine> = f.clone();
    env.storage.db.pool().close().await;
    let unstopped = watch::channel(false).1;
    let key = LayerKey::of(engine.as_ref(), &content);
    let got = build(
        engine.clone(),
        env.storage.clone(),
        key,
        PDF.into(),
        unstopped,
    )
    .await;
    assert!(matches!(got, Err(LayerFailure::Storage(..))), "{got:?}");
    let (stop, stopped) = watch::channel(false);
    stop.send_replace(true);
    let key = LayerKey::of(engine.as_ref(), &content);
    let got = build(engine, env.storage.clone(), key, PDF.into(), stopped).await;
    assert_eq!(got, Err(LayerFailure::Stopped));
    assert_eq!(f.parses.load(SeqCst), 0);
}

/// A parse that fails as its build is told to stop ends stopped, not as
/// the engine's failure.
#[tokio::test]
async fn a_build_stopped_as_its_parse_fails_ends_stopped() {
    let (env, f) = (env().await, fake());
    *f.answer.lock().unwrap() = Some(Err(EngineError::Unsupported));
    let l = layers(&f);
    let stopping = Arc::downgrade(&l);
    *f.then.lock().unwrap() = Some(Arc::new(move || {
        if let Some(l) = stopping.upgrade() {
            stop_tool(&l, "find");
        }
    }));
    let content = stored(&env, "late");
    let got = l
        .layer(&env.storage, &content, PDF, WAIT, tool("find"))
        .await;
    assert_eq!(got, Err(LayerFailure::Stopped));
}
