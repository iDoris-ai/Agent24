#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent24_os_sdk::Usage;
use serde_json::{Value, json};

use super::*;
use crate::extractions::{MissingReason, Status};
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, TextLayer};
use Pass::{Free, Held};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHA: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const MAX: usize = 1 << 20;

/// A model that answers from a script, call by call, and keeps what it
/// was asked: the fields and the blocks of each call.
struct Script {
    replies: Mutex<VecDeque<Result<CompleteResult, ClientError>>>,
    asked: Mutex<Vec<(Vec<String>, Vec<String>)>>,
    passes: Mutex<Vec<Pass>>,
}

impl Script {
    fn new(replies: Vec<Result<CompleteResult, ClientError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            asked: Mutex::default(),
            passes: Mutex::default(),
        }
    }
    fn passes(&self) -> Vec<Pass> {
        self.passes.lock().unwrap().clone()
    }
    fn asked(&self) -> Vec<(Vec<String>, Vec<String>)> {
        self.asked.lock().unwrap().clone()
    }
}

impl Model for Script {
    fn complete(&self, req: CompleteRequest) -> Call {
        // ME4-S2b J8: every extraction call, first or retried, at temperature 0.
        assert_eq!(req.temperature, Some(0.0));
        // J4 (ME4-S2c): the first pass free and told the shape; the second
        // held to the schema, thinking off, not told it.
        let shaped = req.messages[0]
            .content
            .contains("Write your final answer as JSON");
        let pass = if req.response_format.is_some() {
            assert_eq!(req.complexity, Some(Complexity::Simple));
            assert!(!shaped);
            Pass::Held
        } else {
            assert_eq!(req.complexity, None);
            assert!(shaped);
            Pass::Free
        };
        self.passes.lock().unwrap().push(pass);
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

/// An answer that took all the tokens a call may write: cut off, whatever
/// its text.
fn full(text: String) -> Result<CompleteResult, ClientError> {
    let mut r = reply(text);
    r.as_mut().unwrap().usage.completion_tokens = u64::from(MAX_TOKENS);
    r
}

fn present(key: &str, value: &str, block: &str, quote: &str) -> (String, Value) {
    let said = json!({ "value": value, "normalized": "", "evidence": [{ "block_id": block, "quote": quote }] });
    (key.into(), json!({ "present": said }))
}

fn missing(key: &str) -> (String, Value) {
    (
        key.into(),
        json!({ "missing": { "reason": "not_in_document" } }),
    )
}

fn answer(values: Vec<(String, Value)>) -> String {
    json!({ "values": values.into_iter().collect::<serde_json::Map<_, _>>() }).to_string()
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

fn keys(m: &Script) -> Vec<String> {
    m.asked().into_iter().map(|(k, _)| k.join(",")).collect()
}

/// J5: an answer the first pass gets right is the only call; one it does
/// not is asked once more, held.
#[tokio::test]
async fn the_first_pass_answers_or_the_second_is_asked() {
    let l = layer(1, 0);
    let ok = || reply(answer(vec![missing("a")]));
    let m = Script::new(vec![ok()]);
    assert!(go(&m, &l, &[field("a")]).await.is_ok());
    assert_eq!(m.passes(), [Free]);
    let m = Script::new(vec![reply("{\"values\": 1}".into()), ok()]);
    assert!(go(&m, &l, &[field("a")]).await.is_ok());
    assert_eq!(m.passes(), [Free, Held]);
}

/// J7: cut off in the first pass, the batch goes to the second whole; cut
/// off there, its halves stay in the second. A call that used all its tokens
/// is cut off even if its text reads whole, or is empty.
#[tokio::test]
async fn a_cut_off_answer_is_asked_held_then_in_two_halves() {
    let l = layer(1, 0);
    let fields = [field("a"), field("b"), field("c")];
    let whole = answer(vec![missing("a"), missing("b"), missing("c")]);
    let cut = || reply(whole[..whole.len() / 2].to_owned());
    for first in [
        cut(),
        full(whole.clone()),
        full(String::new()),
        full("```json\n{".into()),
        reply("<think>still going".into()),
    ] {
        let m = Script::new(vec![
            first,
            cut(),
            reply(answer(vec![missing("a")])),
            reply(answer(vec![missing("b"), missing("c")])),
        ]);
        assert_eq!(go(&m, &l, &fields).await.unwrap().values.len(), 3);
        assert_eq!(keys(&m), ["a,b,c", "a,b,c", "a", "b,c"]);
        assert_eq!(m.passes(), [Free, Held, Held, Held]);
    }
    // Nothing written and no usage told: cut off, as `parse` reads it.
    let mut none = reply("<think>none of them</think>".into());
    none.as_mut().unwrap().usage.completion_tokens = 0;
    let m = Script::new(vec![
        none,
        cut(),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b"), missing("c")])),
    ]);
    assert!(go(&m, &l, &fields).await.is_ok());
    assert_eq!(m.passes(), [Free, Held, Held, Held]);
    // #1041 L4: the second pass is cut off by its tokens too.
    let m = Script::new(vec![
        cut(),
        full(whole.clone()),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b"), missing("c")])),
    ]);
    assert!(go(&m, &l, &fields).await.is_ok());
    assert_eq!(m.passes(), [Free, Held, Held, Held]);
    // Nothing after its thinking, short of the limit, is malformed, not cut
    // off: cut off held, the halves start again at the first pass.
    let m = Script::new(vec![
        reply("<think>none of them</think>".into()),
        cut(),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b"), missing("c")])),
    ]);
    assert!(go(&m, &l, &fields).await.is_ok());
    assert_eq!(m.passes(), [Free, Held, Free, Free]);
    // Malformed first, cut off held: the halves start again at the first pass.
    let m = Script::new(vec![
        reply("{\"values\": 1}".into()),
        cut(),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b"), missing("c")])),
    ]);
    assert!(go(&m, &l, &fields).await.is_ok());
    assert_eq!(keys(&m), ["a,b,c", "a,b,c", "a", "b,c"]);
    assert_eq!(m.passes(), [Free, Held, Free, Free]);
    // Cut off even for one field, in both passes: the answers are not answers.
    let m = Script::new(vec![
        reply("{\"values\": [".into()),
        reply("{\"values\": [".into()),
    ]);
    assert!(matches!(
        go(&m, &l, &[field("a")]).await,
        Err(Failure::Unanswered(_))
    ));
}

/// J8: malformed held, asked once more held, then a failure; the first
/// pass's malformed answer is not that retry.
#[tokio::test]
async fn a_malformed_answer_is_asked_once_more() {
    let l = layer(1, 0);
    let bad = || reply("{\"values\": 1}".into());
    let m = Script::new(vec![bad(), bad(), reply(answer(vec![missing("a")]))]);
    assert!(go(&m, &l, &[field("a")]).await.is_ok());
    assert_eq!(m.passes(), [Free, Held, Held]);
    let m = Script::new(vec![bad(), bad(), reply("{}".into())]);
    assert!(matches!(
        go(&m, &l, &[field("a")]).await,
        Err(Failure::Unanswered(_))
    ));
    assert_eq!(m.passes(), [Free, Held, Held]);
}

fn rejected() -> Result<CompleteResult, ClientError> {
    Err(ClientError::Unavailable {
        retryable: false,
        cause: UnavailableCause::RequestRejected,
    })
}

/// J8b: rejected in the first pass, the same window is asked held; rejected
/// there too, it is asked in halves, each from the first pass. Rejected held
/// after a first pass was taken fails, with no call after it.
#[tokio::test]
async fn a_window_the_model_rejects_is_asked_held_then_in_halves() {
    let l = layer(2, 0);
    let date = || {
        reply(answer(vec![present(
            "date",
            "10月12日",
            "p1/b1",
            "10月12日",
        )]))
    };
    let blocks =
        |m: &Script| -> Vec<String> { m.asked().into_iter().map(|(_, b)| b.join(",")).collect() };
    let m = Script::new(vec![rejected(), date()]);
    assert!(go(&m, &l, &[field("date")]).await.is_ok());
    assert_eq!(blocks(&m), ["p1/b1,p2/b1", "p1/b1,p2/b1"]);
    assert_eq!(m.passes(), [Free, Held]);
    let m = Script::new(vec![
        rejected(),
        rejected(),
        date(),
        reply(answer(vec![missing("date")])),
    ]);
    let out = go(&m, &l, &[field("date")]).await.unwrap();
    assert_eq!(out.values[0].value.status, Status::Present);
    assert_eq!(blocks(&m), ["p1/b1,p2/b1", "p1/b1,p2/b1", "p1/b1", "p2/b1"]);
    assert_eq!(m.passes(), [Free, Held, Free, Free]);
    // A half window starts afresh: rejected in both passes, it is halved
    // again, for no pass was taken on it.
    let m = Script::new(vec![
        rejected(),
        rejected(),
        rejected(),
        rejected(),
        reply(answer(vec![missing("date")])),
        reply(answer(vec![missing("date")])),
        reply(answer(vec![missing("date")])),
    ]);
    assert!(go(&m, &layer(4, 0), &[field("date")]).await.is_ok());
    assert_eq!(
        blocks(&m)[2..],
        [
            "p1/b1,p2/b1",
            "p1/b1,p2/b1",
            "p1/b1",
            "p2/b1",
            "p3/b1,p4/b1"
        ]
    );
    // One block it still rejects: no model can read it.
    let m = Script::new(vec![rejected(), rejected()]);
    assert!(matches!(
        go(&m, &layer(1, 0), &[field("a")]).await,
        Err(Failure::Engine(_))
    ));
    // Rejected, then cut off held: the halves stay held.
    let fields = [field("a"), field("b")];
    let both = answer(vec![missing("a"), missing("b")]);
    let m = Script::new(vec![
        rejected(),
        reply(both[..10].to_owned()),
        reply(answer(vec![missing("a")])),
        reply(answer(vec![missing("b")])),
    ]);
    assert!(go(&m, &layer(1, 0), &fields).await.is_ok());
    assert_eq!(m.passes(), [Free, Held, Held, Held]);
    // A first pass taken, then the second rejected: no window split.
    for first in [
        reply("{\"values\": 1}".into()),
        reply(both[..10].to_owned()),
    ] {
        let m = Script::new(vec![first, rejected()]);
        let out = go(&m, &l, &fields).await;
        assert_eq!(
            out,
            Err(Failure::Engine(
                "the local model rejected the second pass".into()
            ))
        );
        assert_eq!(m.passes(), [Free, Held]);
    }
    // So too for a half held because its batch thought too long.
    let m = Script::new(vec![
        reply(both[..10].to_owned()),
        reply(both[..10].to_owned()),
        rejected(),
    ]);
    assert!(matches!(go(&m, &l, &fields).await, Err(Failure::Engine(_))));
    assert_eq!(m.passes(), [Free, Held, Held]);
    // Rejected first, then taken held (malformed), then rejected held: not
    // a window too long either.
    let m = Script::new(vec![
        rejected(),
        reply("{\"values\": 1}".into()),
        rejected(),
    ]);
    assert!(matches!(go(&m, &l, &fields).await, Err(Failure::Engine(_))));
    assert_eq!(m.passes(), [Free, Held, Held]);
    // Rejected first, cut off held, so its halves stay held: one rejected
    // there fails too, for this window took a second pass.
    let m = Script::new(vec![rejected(), reply(both[..10].to_owned()), rejected()]);
    assert!(matches!(go(&m, &l, &fields).await, Err(Failure::Engine(_))));
    assert_eq!(m.passes(), [Free, Held, Held]);
    // Malformed, then cut off held: the halves start at the first pass, but
    // this window took a pass, so a half rejected in both fails too.
    let m = Script::new(vec![
        reply("{\"values\": 1}".into()),
        reply(both[..10].to_owned()),
        rejected(),
        rejected(),
    ]);
    assert_eq!(
        go(&m, &l, &fields).await,
        Err(Failure::Engine(
            "the local model rejected the second pass".into()
        ))
    );
    assert_eq!(m.passes(), [Free, Held, Free, Held]);
    // Timed out twice in the first pass: a failure, not a second pass.
    let timeout = || Err(ClientError::Timeout("t".into()));
    let m = Script::new(vec![timeout(), timeout()]);
    assert!(matches!(go(&m, &l, &fields).await, Err(Failure::Engine(_))));
    assert_eq!(m.passes(), [Free, Free]);
}

/// The pace in milliseconds: 10, doubling to 40, for at most 200.
const FAST: Pace = Pace {
    first: Duration::from_millis(10),
    longest: Duration::from_millis(40),
    max_wait: Duration::from_millis(200),
    no_provider: Duration::from_millis(100),
    no_provider_max: Duration::from_millis(300),
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
    // Codex R1: the last wait is what is left of the budget. With pauses of
    // 10, 20, 40, 80, 160 and then 400, the sixth is cut to 190: 500 ms in
    // all, where an uncut one would make 630. The gap is wide enough for a
    // busy machine's late wake-ups.
    let budget = Pace {
        first: Duration::from_millis(10),
        longest: Duration::from_millis(400),
        max_wait: Duration::from_millis(500),
        no_provider: Duration::from_millis(100),
        no_provider_max: Duration::from_millis(300),
    };
    let m = Script::new((0..20).map(|_| busy()).collect());
    let start = std::time::Instant::now();
    let out = paced(
        budget,
        &Steps::default(),
        &m,
        &source(&l),
        &[field("a")],
        false,
        MAX,
    )
    .await;
    assert!(matches!(out, Err(Failure::Engine(_))));
    let waited = start.elapsed();
    assert!(
        waited >= budget.max_wait && waited < Duration::from_millis(600),
        "{waited:?}"
    );
    assert_eq!(m.asked().len(), 7);
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
            retryable: false,
            cause: UnavailableCause::BackendConfig,
        },
        ClientError::InvalidParams("p".into()),
        // Only no_provider is asked again; this one is not.
        ClientError::Unavailable {
            retryable: false,
            cause: UnavailableCause::ResponseTooLarge,
        },
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
    let long = "m".repeat(257);
    for (replies, what) in [
        (vec![remote], "remote"),
        (vec![nameless], "no id"),
        (vec![from(&long, a.clone())], "an id over 256 bytes"),
        // Bytes, not characters: 86 of 模 are 258 bytes.
        (
            vec![from(&"模".repeat(86), a.clone())],
            "a short id over 256 bytes",
        ),
        (vec![from("m\0", a.clone())], "an id with a NUL"),
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
        &Script::new(vec![from("m1", a.clone()), from("m1", a.clone())]),
        &l,
        &[field("a")],
    )
    .await
    .unwrap();
    assert_eq!(out.model_id, "m1");
    let longest = "模".repeat(85) + "m";
    let replies = vec![from(&longest, a.clone()), from(&longest, a)];
    let out = go(&Script::new(replies), &l, &[field("a")]).await;
    assert_eq!(out.map(|e| e.model_id), Ok(longest));
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
    // Rejected over two pages, in both passes; then the first page's answer
    // is cut off in both, and asked held in two halves of the batch; the
    // second page answers whole in its first pass.
    let l = layer(2, 0);
    let fields = [field("a"), field("b")];
    let both = answer(vec![missing("a"), missing("b")]);
    let cut = || reply(both[..both.len() - 3].to_owned());
    let m = Script::new(vec![
        rejected(),
        rejected(),
        cut(),
        cut(),
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
            ("a,b".to_owned(), "p1/b1,p2/b1".to_owned()),
            ("a,b".to_owned(), "p1/b1".to_owned()),
            ("a,b".to_owned(), "p1/b1".to_owned()),
            ("a".to_owned(), "p1/b1".to_owned()),
            ("b".to_owned(), "p1/b1".to_owned()),
            ("a,b".to_owned(), "p2/b1".to_owned()),
        ]
    );
    assert_eq!(m.passes(), [Free, Held, Free, Held, Held, Held, Free]);
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
        let mut first: Vec<(String, Value)> =
            fields[..20].iter().map(|f| missing(&f.key)).collect();
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

/// J14 (ME4-S2a §2.6): no local model to answer is asked again after the
/// router's longest cooldown, for up to three of them; the wait ends when the
/// job is cancelled.
#[tokio::test]
async fn no_local_model_is_asked_again_after_the_longest_cooldown() {
    let l = layer(1, 0);
    let none = || {
        Err(ClientError::Unavailable {
            retryable: true,
            cause: UnavailableCause::NoProvider,
        })
    };
    // Once, then an answer: the extraction goes on, after the wait.
    let m = Script::new(vec![none(), reply(answer(vec![missing("a")]))]);
    let start = std::time::Instant::now();
    let out = paced(
        FAST,
        &Steps::default(),
        &m,
        &source(&l),
        &[field("a")],
        false,
        MAX,
    )
    .await;
    assert!(out.is_ok(), "{out:?}");
    assert!(start.elapsed() >= FAST.no_provider);
    assert_eq!(m.asked.lock().unwrap().len(), 2);
    // A cooldown another call re-armed: three waits, then an answer.
    let m = Script::new(vec![
        none(),
        none(),
        none(),
        reply(answer(vec![missing("a")])),
    ]);
    let out = paced(
        FAST,
        &Steps::default(),
        &m,
        &source(&l),
        &[field("a")],
        false,
        MAX,
    )
    .await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(m.asked.lock().unwrap().len(), 4);
    // Past the budget: no local model is a failure of the engine.
    let m = Script::new(vec![none(), none(), none(), none()]);
    let start = std::time::Instant::now();
    let out = paced(
        FAST,
        &Steps::default(),
        &m,
        &source(&l),
        &[field("a")],
        false,
        MAX,
    )
    .await;
    assert!(matches!(out, Err(Failure::Engine(_))), "{out:?}");
    assert_eq!(m.asked.lock().unwrap().len(), 4);
    // The budget is what it waits: not less, and not twice it.
    let elapsed = start.elapsed();
    assert!(elapsed >= FAST.no_provider_max, "{elapsed:?}");
    assert!(elapsed < FAST.no_provider_max * 2, "{elapsed:?}");
    // Cancelled while it waits: it ends then, without asking again.
    let m = Script::new(vec![none(), reply(answer(vec![missing("a")]))]);
    let job = Steps {
        stop: Some(3),
        ..Steps::default()
    };
    // A wait far longer than the cancel takes, so a late wake-up cannot pass for it.
    let long = Pace {
        no_provider: Duration::from_secs(5),
        no_provider_max: Duration::from_secs(15),
        ..FAST
    };
    let start = std::time::Instant::now();
    let out = paced(long, &job, &m, &source(&l), &[field("a")], false, MAX).await;
    assert_eq!(out, Err(Failure::Cancelled));
    assert_eq!(m.asked.lock().unwrap().len(), 1);
    assert!(start.elapsed() < Duration::from_secs(2));
}

/// A call's budgets for a busy kernel, a timeout and no local model are each
/// their own: spending one does not reset another.
#[tokio::test]
async fn a_calls_budgets_are_each_their_own() {
    let l = &layer(1, 0);
    let none = || {
        Err(ClientError::Unavailable {
            retryable: true,
            cause: UnavailableCause::NoProvider,
        })
    };
    let busy = || Err(ClientError::Busy("b".into()));
    let timeout = || Err(ClientError::Timeout("t".into()));
    let run = |replies| async move {
        let m = Script::new(replies);
        let out = paced(
            FAST,
            &Steps::default(),
            &m,
            &source(l),
            &[field("a")],
            false,
            MAX,
        )
        .await;
        (out, m.asked().len())
    };
    // Mixed, within every budget: the answer comes.
    let ok = || reply(answer(vec![missing("a")]));
    let (out, asked) = run(vec![
        none(),
        busy(),
        timeout(),
        none(),
        busy(),
        none(),
        ok(),
    ])
    .await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(asked, 7);
    // A second timeout, with other failures between, still fails the call.
    let (out, asked) = run(vec![timeout(), none(), busy(), timeout()]).await;
    assert!(matches!(out, Err(Failure::Engine(_))), "{out:?}");
    assert_eq!(asked, 4);
    // So does a fourth no_provider, with a busy kernel and a timeout between.
    let (out, asked) = run(vec![none(), busy(), none(), timeout(), none(), none()]).await;
    assert!(matches!(out, Err(Failure::Engine(_))), "{out:?}");
    assert_eq!(asked, 6);
    // And a busy kernel past its budget, with a no_provider wait between:
    // six waits spend 190 of 200 ms, the eighth call spends the rest.
    let mut replies: Vec<_> = (0..6).map(|_| busy()).collect();
    replies.extend([none(), busy(), busy(), ok()]);
    let (out, asked) = run(replies).await;
    assert!(matches!(out, Err(Failure::Engine(_))), "{out:?}");
    assert_eq!(asked, 9);
}
