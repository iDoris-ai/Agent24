#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent24_os_sdk::Usage;
use serde_json::{Value, json};

use super::*;
use crate::extractions::{MissingReason, Status};
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, TextLayer};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHA: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const MAX: usize = 1 << 20;

/// A model that answers from a script, call by call, and keeps what it
/// was asked: the fields and the blocks of each call.
struct Script {
    replies: Mutex<VecDeque<Result<CompleteResult, ClientError>>>,
    asked: Mutex<Vec<(Vec<String>, Vec<String>)>>,
}

impl Script {
    fn new(replies: Vec<Result<CompleteResult, ClientError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            asked: Mutex::default(),
        }
    }
    fn asked(&self) -> Vec<(Vec<String>, Vec<String>)> {
        self.asked.lock().unwrap().clone()
    }
}

impl Model for Script {
    fn complete(&self, req: CompleteRequest) -> Call {
        let data: Value = serde_json::from_str(&req.messages[1].content).unwrap();
        let keys = data["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["key"].as_str().unwrap().to_owned())
            .collect();
        let blocks = data["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["block_id"].as_str().unwrap().to_owned())
            .collect();
        self.asked.lock().unwrap().push((keys, blocks));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a call past the script");
        Box::pin(async move { reply })
    }
}

fn reply(text: String) -> Result<CompleteResult, ClientError> {
    from("qwen3-8b", text)
}

fn from(model: &str, text: String) -> Result<CompleteResult, ClientError> {
    Ok(CompleteResult {
        text,
        model_id: Some(model.into()),
        tier: ServedTier::Local,
        usage: Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
        },
    })
}

fn present(key: &str, value: &str, block: &str, quote: &str) -> Value {
    json!({ "key": key, "status": "present", "value": value, "normalized": "", "evidence": [{ "block_id": block, "quote": quote }],
            "missing_reason": "", "candidates": [] })
}

fn missing(key: &str) -> Value {
    json!({ "key": key, "status": "missing", "value": "", "normalized": "", "evidence": [],
            "missing_reason": "not_in_document", "candidates": [] })
}

fn answer(values: Vec<Value>) -> String {
    json!({ "values": values }).to_string()
}

fn field(key: &str) -> Field {
    Field {
        key: key.into(),
        description: key.into(),
        kind: None,
    }
}

/// A layer of `pages` pages, one block each, `size` bytes of text, the
/// first reading "停水 10月12日".
fn layer(pages: u32, size: usize) -> TextLayer {
    let blocks = (1..=pages)
        .map(|p| {
            let text = if p == 1 {
                format!("停水 10月12日{}", " ".repeat(size))
            } else {
                "x".repeat(size)
            };
            Block {
                block_id: format!("p{p}/b1"),
                page: p,
                lines: vec![Line {
                    start: 0,
                    end: text.len(),
                    rect: [0.0, 0.0, 1.0, 1.0],
                }],
                text,
            }
        })
        .collect();
    TextLayer {
        v: 1,
        content_sha256: SHA.into(),
        engine: EngineRef {
            id: "e".into(),
            version: "1".into(),
        },
        config: json!({}),
        pages,
        parse_status: ParseStatus::Complete,
        unparsed_regions: vec![],
        blocks,
    }
}

fn source(l: &TextLayer) -> Source<'_> {
    Source {
        document_id: DOC,
        revision: 1,
        content_sha256: SHA,
        media_type: "application/pdf",
        text_layer_sha256: SHA,
        layer: l,
    }
}

/// A job that goes on until its `stop`-th check, and keeps its progress.
#[derive(Default)]
struct Steps {
    stop: Option<usize>,
    checks: AtomicUsize,
    seen: Mutex<Vec<(usize, usize)>>,
}

impl Job for Steps {
    fn go_on(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        let n = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { self.stop.is_none_or(|stop| n < stop) })
    }
    fn progress(&self, done: usize, total: usize) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.seen.lock().unwrap().push((done, total));
        Box::pin(async {})
    }
}

async fn go(model: &Script, l: &TextLayer, fields: &[Field]) -> Result<Extracted, Failure> {
    run(&Steps::default(), model, &source(l), fields, false, MAX).await
}

#[tokio::test]
async fn one_call_reads_the_layer_and_each_field_is_grounded() {
    let l = layer(1, 0);
    let m = Script::new(vec![reply(answer(vec![
        present("date", "10月12日", "p1/b1", "停水 10月12日"),
        missing("fee"),
    ]))]);
    let job = Steps::default();
    let out = run(
        &job,
        &m,
        &source(&l),
        &[field("date"), field("fee")],
        false,
        MAX,
    )
    .await
    .unwrap();
    assert_eq!(out.model_id, "qwen3-8b");
    let [date, fee] = &out.values[..] else {
        panic!()
    };
    assert_eq!(
        (date.value.status, date.value.anchors.as_ref().map(Vec::len)),
        (Status::Present, Some(1))
    );
    assert_eq!(fee.value.missing_reason, Some(MissingReason::NotInDocument));
    assert_eq!(*job.seen.lock().unwrap(), [(1, 1)]);
    assert_eq!(
        m.asked(),
        [(
            vec!["date".to_owned(), "fee".to_owned()],
            vec!["p1/b1".to_owned()]
        )]
    );
}

#[tokio::test]
async fn a_cut_off_answer_is_asked_again_in_two_halves() {
    let l = layer(1, 0);
    let whole = answer(vec![missing("a"), missing("b"), missing("c")]);
    let m = Script::new(vec![
        reply(whole[..whole.len() / 2].to_owned()),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b"), missing("c")])),
    ]);
    let out = go(&m, &l, &[field("a"), field("b"), field("c")])
        .await
        .unwrap();
    assert_eq!(out.values.len(), 3);
    let asked: Vec<Vec<String>> = m.asked().into_iter().map(|(k, _)| k).collect();
    assert_eq!(asked, [vec!["a", "b", "c"], vec!["a"], vec!["b", "c"]]);
    // Cut off even for one field: the answers are not answers.
    let m = Script::new(vec![reply("{\"values\": [".into())]);
    assert!(matches!(
        go(&m, &l, &[field("a")]).await,
        Err(Failure::Unanswered(_))
    ));
}

#[tokio::test]
async fn a_malformed_answer_is_asked_once_more() {
    let l = layer(1, 0);
    let m = Script::new(vec![
        reply("{\"values\": 1}".into()),
        reply(answer(vec![missing("a")])),
    ]);
    assert!(go(&m, &l, &[field("a")]).await.is_ok());
    let m = Script::new(vec![reply("{\"values\": 1}".into()), reply("{}".into())]);
    assert!(matches!(
        go(&m, &l, &[field("a")]).await,
        Err(Failure::Unanswered(_))
    ));
}

fn rejected() -> Result<CompleteResult, ClientError> {
    Err(ClientError::Unavailable {
        retryable: false,
        cause: UnavailableCause::RequestRejected,
    })
}

#[tokio::test]
async fn a_window_the_model_rejects_is_asked_in_halves() {
    let l = layer(2, 0);
    let m = Script::new(vec![
        rejected(),
        reply(answer(vec![present(
            "date",
            "10月12日",
            "p1/b1",
            "10月12日",
        )])),
        reply(answer(vec![missing("date")])),
    ]);
    let out = go(&m, &l, &[field("date")]).await.unwrap();
    assert_eq!(out.values[0].value.status, Status::Present);
    let blocks: Vec<Vec<String>> = m.asked().into_iter().map(|(_, b)| b).collect();
    assert_eq!(
        blocks,
        [vec!["p1/b1", "p2/b1"], vec!["p1/b1"], vec!["p2/b1"]]
    );
    // One block it still rejects: no model can read it.
    let m = Script::new(vec![rejected()]);
    assert!(matches!(
        go(&m, &layer(1, 0), &[field("a")]).await,
        Err(Failure::Engine(_))
    ));
}

/// The pace in milliseconds: 10, doubling to 40, for at most 200.
const FAST: Pace = Pace {
    first: Duration::from_millis(10),
    longest: Duration::from_millis(40),
    max_wait: Duration::from_millis(200),
};

async fn fast(model: &Script, l: &TextLayer) -> Result<Extracted, Failure> {
    paced(
        FAST,
        &Steps::default(),
        model,
        &source(l),
        &[field("a")],
        false,
        MAX,
    )
    .await
}

#[tokio::test]
async fn a_busy_kernel_is_waited_out_for_a_while_and_a_timeout_retried_once() {
    let l = layer(1, 0);
    let ok = || reply(answer(vec![missing("a")]));
    let busy = || Err(ClientError::Busy("b".into()));
    let m = Script::new(vec![
        busy(),
        Err(ClientError::RateLimited("r".into())),
        ok(),
    ]);
    let start = std::time::Instant::now();
    assert!(fast(&m, &l).await.is_ok());
    assert!(
        start.elapsed() >= Duration::from_millis(30),
        "10 ms, then 20"
    );
    // Waits of 10, 20, 40, 40, 40, 40 and the 10 left make 200 ms: the
    // eighth answer is the last.
    let m = Script::new((0..20).map(|_| busy()).collect());
    let start = std::time::Instant::now();
    assert!(matches!(fast(&m, &l).await, Err(Failure::Engine(_))));
    // Codex R1: 10 + 20 + 40 × 4 + the 10 left is 200, never 230.
    let waited = start.elapsed();
    assert!(
        waited >= FAST.max_wait && waited < FAST.max_wait + Duration::from_millis(25),
        "{waited:?}"
    );
    assert_eq!(m.asked().len(), 8);
    let timeout = || Err(ClientError::Timeout("t".into()));
    assert!(fast(&Script::new(vec![timeout(), ok()]), &l).await.is_ok());
    assert!(matches!(
        fast(&Script::new(vec![timeout(), timeout()]), &l).await,
        Err(Failure::Engine(_))
    ));
    // The real pace waits up to two minutes, pausing up to 30 s.
    assert_eq!(
        (PACE.first, PACE.longest, PACE.max_wait),
        (
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(120)
        )
    );
}

#[tokio::test]
async fn each_failure_of_the_kernel_has_its_outcome() {
    let l = layer(1, 0);
    for (e, want) in [
        (ClientError::Cancelled, Failure::Interrupted),
        (ClientError::Draining("d".into()), Failure::Interrupted),
        (ClientError::ConnectionLost, Failure::Interrupted),
    ] {
        assert_eq!(
            go(&Script::new(vec![Err(e)]), &l, &[field("a")]).await,
            Err(want)
        );
    }
    for e in [
        ClientError::Forbidden("f".into()),
        ClientError::Unavailable {
            retryable: true,
            cause: UnavailableCause::NoProvider,
        },
        ClientError::Unavailable {
            retryable: false,
            cause: UnavailableCause::BackendConfig,
        },
        ClientError::InvalidParams("p".into()),
    ] {
        assert!(matches!(
            go(&Script::new(vec![Err(e)]), &l, &[field("a")]).await,
            Err(Failure::Engine(_))
        ));
    }
}

#[tokio::test]
async fn every_call_names_one_local_model() {
    let l = layer(2, 30 * 1024);
    let a = answer(vec![missing("a")]);
    let mut remote = from("m", a.clone());
    remote.as_mut().unwrap().tier = ServedTier::Remote;
    let mut nameless = from("m", a.clone());
    nameless.as_mut().unwrap().model_id = None;
    for (replies, what) in [
        (vec![remote], "remote"),
        (vec![nameless], "no id"),
        (
            vec![from("m1", a.clone()), from("m2", a.clone())],
            "changed",
        ),
    ] {
        assert!(
            matches!(
                go(&Script::new(replies), &l, &[field("a")]).await,
                Err(Failure::Engine(_))
            ),
            "{what}"
        );
    }
    let out = go(
        &Script::new(vec![from("m1", a.clone()), from("m1", a)]),
        &l,
        &[field("a")],
    )
    .await
    .unwrap();
    assert_eq!(out.model_id, "m1");
}

#[tokio::test]
async fn a_cancelled_job_stops_before_its_next_call() {
    let l = layer(3, 20 * 1024);
    let m = Script::new((0..3).map(|_| reply(answer(vec![missing("a")]))).collect());
    let job = Steps {
        stop: Some(3),
        ..Steps::default()
    };
    let out = run(&job, &m, &source(&l), &[field("a")], false, MAX).await;
    assert_eq!(out, Err(Failure::Cancelled));
    assert_eq!(*job.seen.lock().unwrap(), [(1, 3), (2, 3)]);
    assert_eq!(m.asked().len(), 2);
    // Codex R1: also inside a split. A batch of four cut off again and
    // again is stopped at its next call, not after the whole tree.
    let whole = answer((0..4).map(|n| missing(&format!("f{n}"))).collect());
    let cut = || reply(whole[..10].to_owned());
    let m = Script::new((0..10).map(|_| cut()).collect());
    let job = Steps {
        stop: Some(3),
        ..Steps::default()
    };
    let fields: Vec<Field> = (0..4).map(|n| field(&format!("f{n}"))).collect();
    let out = run(&job, &m, &source(&layer(1, 0)), &fields, false, MAX).await;
    assert_eq!(out, Err(Failure::Cancelled));
    assert_eq!(m.asked().len(), 2);
    assert!(job.seen.lock().unwrap().is_empty());
    // Codex R2: before a wait too. Cancelled while the kernel was busy:
    // stopped at once, not after a pause of up to 30 s.
    let m = Script::new(vec![Err(ClientError::Busy("b".into()))]);
    let job = Steps {
        stop: Some(2),
        ..Steps::default()
    };
    let start = std::time::Instant::now();
    let out = run(&job, &m, &source(&layer(1, 0)), &[field("a")], false, MAX).await;
    assert_eq!(out, Err(Failure::Cancelled));
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn a_window_split_and_a_batch_split_mix() {
    // Rejected over two pages; then the first page's answer is cut off and
    // asked in two halves of the batch; the second page answers whole.
    let l = layer(2, 0);
    let fields = [field("a"), field("b")];
    let both = answer(vec![missing("a"), missing("b")]);
    let m = Script::new(vec![
        rejected(),
        reply(both[..both.len() - 3].to_owned()),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b")])),
        reply(both.clone()),
    ]);
    let out = go(&m, &l, &fields).await.unwrap();
    assert_eq!(out.values.len(), 2);
    let asked: Vec<(Vec<String>, Vec<String>)> = m.asked();
    assert_eq!(
        asked
            .iter()
            .map(|(k, b)| (k.join(","), b.join(",")))
            .collect::<Vec<_>>(),
        [
            ("a,b".to_owned(), "p1/b1,p2/b1".to_owned()),
            ("a,b".to_owned(), "p1/b1".to_owned()),
            ("a".to_owned(), "p1/b1".to_owned()),
            ("b".to_owned(), "p1/b1".to_owned()),
            ("a,b".to_owned(), "p2/b1".to_owned()),
        ]
    );
}

/// The whole run can move between threads, as a job's work must.
#[test]
fn a_run_is_send() {
    fn send<T: Send>(_: T) {}
    let l = layer(1, 0);
    let (m, job) = (Script::new(vec![]), Steps::default());
    let src = source(&l);
    let fields = [field("a")];
    send(run(&job, &m, &src, &fields, false, MAX));
}

#[tokio::test]
async fn windows_and_batches_each_answer_and_merge_per_field() {
    // Three windows, 21 fields in batches of 20 and 1: six calls.
    let l = layer(3, 20 * 1024);
    let fields: Vec<Field> = (0..21).map(|n| field(&format!("f{n}"))).collect();
    let batch = |keys: &[Field]| answer(keys.iter().map(|f| missing(&f.key)).collect());
    let mut replies = Vec::new();
    for w in 0..3 {
        let mut first: Vec<Value> = fields[..20].iter().map(|f| missing(&f.key)).collect();
        if w == 2 {
            first[0] = present("f0", "x", "p3/b1", "xxxx");
        }
        replies.push(reply(answer(first)));
        replies.push(reply(batch(&fields[20..])));
    }
    let m = Script::new(replies);
    let out = go(&m, &l, &fields).await.unwrap();
    assert_eq!(m.asked().len(), 6);
    assert_eq!(
        out.values
            .iter()
            .map(|v| v.value.key.as_str())
            .collect::<Vec<_>>(),
        fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>()
    );
    // Present in the third window, missing in the others: present, but the
    // quote occurs many times in its block, so it is unsourced.
    assert_eq!(out.values[0].value.status, Status::Present);
    assert_eq!(out.values[0].value.anchors, Some(vec![]));
}
