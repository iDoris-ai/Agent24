#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{CompletionRequest, ModelError, ModelProvider, Msg, OpenAiCompatProvider, Progress};

/// One step of a backend's answer: wait, then write.
type Step = (u64, String);

/// A backend that answers one request: its headers at once, then each step
/// of `script` in turn, then it closes. What it was asked comes back.
async fn backend(
    content_type: &'static str,
    script: Vec<Step>,
) -> (String, tokio::task::JoinHandle<String>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let asked = request(&mut s).await;
        let head =
            format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n");
        if s.write_all(head.as_bytes()).await.is_err() {
            return asked;
        }
        for (pause, bytes) in script {
            tokio::time::sleep(Duration::from_millis(pause)).await;
            if s.write_all(bytes.as_bytes()).await.is_err() {
                break;
            }
        }
        asked
    });
    (url, task)
}

/// A backend that takes the request and says nothing, for as long as it is kept.
async fn silent() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _ = request(&mut s).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    url
}

async fn request(s: &mut tokio::net::TcpStream) -> String {
    let mut got = Vec::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = s.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&got).to_string();
        if let Some(i) = text.find("\r\n\r\n") {
            let len: usize = text
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if got.len() >= i + 4 + len || n == 0 {
                return text[i + 4..].to_string();
            }
        }
    }
}

fn event(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn token(t: &str) -> String {
    event(&format!(
        r#"{{"model":"stub-7b","choices":[{{"index":0,"delta":{{"content":"{t}"}},"finish_reason":null}}]}}"#
    ))
}

fn finish() -> String {
    event(r#"{"model":"stub-7b","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)
}

fn usage(completion: u32) -> String {
    // A later event may name another model: the first one named is the answer's.
    event(&format!(
        r#"{{"model":"later","choices":[],"usage":{{"prompt_tokens":5,"completion_tokens":{completion},"total_tokens":{}}}}}"#,
        5 + completion
    ))
}

/// Timers in milliseconds: the first chunk, a stall, and the deadline from now.
fn progress(first: u64, stall: u64, deadline: u64) -> Progress {
    Progress {
        deadline: Instant::now() + Duration::from_millis(deadline),
        first_chunk: Duration::from_millis(first),
        stall: Duration::from_millis(stall),
    }
}

fn req(p: Option<Progress>) -> CompletionRequest {
    CompletionRequest {
        messages: vec![Msg::user("hi")],
        model: None,
        tools: vec![],
        response_format: None,
        max_tokens: None,
        disable_thinking: false,
        idoris_headroom_confirmed: false,
        temperature: None,
        progress: p,
    }
}

fn provider(url: String) -> OpenAiCompatProvider {
    OpenAiCompatProvider::new("ollama", url, None, "local", "qwen2.5:7b")
}

async fn unavailable(r: Result<crate::CompletionResponse, ModelError>) -> String {
    match r {
        Err(ModelError::Unavailable(m)) => m,
        other => panic!("not Unavailable: {other:?}"),
    }
}

#[tokio::test]
async fn an_answer_that_keeps_coming_is_read_whole_however_long_it_takes() {
    // 20 tokens 50ms apart: a second in all, past a fixed 300ms that the timers never allow.
    let mut script: Vec<Step> = vec![(
        0,
        event(r#"{"model":"stub-7b","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#),
    )];
    script.extend((0..20).map(|i| (50, token(&format!("t{i} ")))));
    script.extend([
        (10, finish()),
        (10, usage(1)),
        (10, usage(20)),
        (10, "data: [DONE]\n\n".to_owned()),
    ]);
    let (url, asked) = backend("text/event-stream", script).await;
    let started = Instant::now();
    // The provider's own total timeout is 300ms: a streamed call is not bound by it.
    // The stall timer is far above the 50ms between tokens, so a slow CI
    // machine's late writes cannot pass for a stall (it did, at 150ms).
    let r = provider(url)
        .with_timeouts(Duration::from_millis(300), Duration::from_millis(300))
        .complete(
            &req(Some(progress(1000, 1000, 10_000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(started.elapsed() > Duration::from_millis(900));
    let text: String = (0..20).map(|i| format!("t{i} ")).collect();
    assert_eq!(r.message.content.as_deref(), Some(text.as_str()));
    assert_eq!(
        (r.message.role.as_str(), r.message.tool_calls.len()),
        ("assistant", 0)
    );
    assert_eq!(r.model_id.as_deref(), Some("stub-7b"));
    assert_eq!(
        (
            r.usage.prompt_tokens,
            r.usage.completion_tokens,
            r.usage.total_tokens
        ),
        (5, 20, 25)
    );
    let body: serde_json::Value = serde_json::from_str(&asked.await.unwrap()).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["stream_options"],
        serde_json::json!({ "include_usage": true })
    );
}

#[tokio::test]
async fn without_progress_the_request_is_todays() {
    let body = r#"{"model":"m","choices":[{"message":{"role":"assistant","content":"x"}}]}"#;
    let (url, asked) = backend("application/json", vec![(0, body.to_owned())]).await;
    provider(url)
        .complete(&req(None), &CancellationToken::new())
        .await
        .unwrap();
    let sent: serde_json::Value = serde_json::from_str(&asked.await.unwrap()).unwrap();
    assert_eq!(sent["stream"], false);
    assert!(sent.get("stream_options").is_none());
}

#[tokio::test]
async fn a_stall_after_output_is_found_within_the_stall_timer() {
    let (url, _) = backend("text/event-stream", vec![(0, token("a")), (3000, finish())]).await;
    let started = Instant::now();
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output for"), "{m}");
    assert!(started.elapsed() < Duration::from_millis(1000));
}

#[tokio::test]
async fn no_first_chunk_is_found_within_the_first_chunk_timer() {
    let url = silent().await;
    let started = Instant::now();
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output within"), "{m}");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(280) && took < Duration::from_millis(1000),
        "{took:?}"
    );
}

#[tokio::test]
async fn output_that_trickles_on_past_the_deadline_is_cut_there() {
    // Every 100ms, well within the 1s stall, for 6s: the 3s deadline ends it.
    // Both timers stay below the deadline, so a stream that stalled or never
    // started would be named so, not cut at the deadline.
    let script: Vec<Step> = (0..60).map(|i| (100, token(&i.to_string()))).collect();
    let (url, _) = backend("text/event-stream", script).await;
    let started = Instant::now();
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(2000, 1000, 3000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("deadline"), "{m}");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2950) && took < Duration::from_millis(5000),
        "{took:?}"
    );
}

#[tokio::test]
async fn comments_and_other_fields_move_no_timer() {
    let keep: Vec<Step> = (0..40)
        .map(|_| (50, ": keep-alive\n\nevent: ping\nid: 1\n\n".to_owned()))
        .collect();
    let (url, _) = backend("text/event-stream", keep.clone()).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output within"), "{m}");
    let mut after = vec![(0, token("a"))];
    after.extend(keep);
    let (url, _) = backend("text/event-stream", after).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output for"), "{m}");
    // An empty data event is no output either.
    let mut empty = vec![(0, token("a"))];
    empty.extend((0..40).map(|_| (50, "data:\n\n".to_owned())));
    let (url, _) = backend("text/event-stream", empty).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output for"), "{m}");
}

#[tokio::test]
async fn a_stream_that_breaks_off_is_no_answer_but_one_that_finished_is() {
    let (url, _) = backend("text/event-stream", vec![(0, token("half"))]).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("ended before"), "{m}");
    // A finish_reason, then the end without [DONE]: the answer is whole.
    let (url, _) = backend(
        "text/event-stream",
        vec![(0, token("whole")), (0, finish())],
    )
    .await;
    let r = provider(url)
        .complete(
            &req(Some(progress(300, 150, 5000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.message.content.as_deref(), Some("whole"));
    // An event written on two data lines is one value, joined by a line break:
    // inside a JSON string, that break makes it no JSON.
    let torn = "data: {\"choices\":[{\"delta\":{\"content\":\"a\ndata: b\"}}]}\n\n".to_owned();
    let (url, _) = backend("text/event-stream", vec![(0, torn)]).await;
    match provider(url)
        .complete(
            &req(Some(progress(300, 150, 5000))),
            &CancellationToken::new(),
        )
        .await
    {
        Err(ModelError::Provider(m)) => assert!(m.contains("not JSON"), "{m}"),
        other => panic!("{other:?}"),
    }
    // An event written on two data lines is one value, joined by a line break.
    let split = "data: {\"model\":\"m\",\r\ndata: \"choices\":[{\"delta\":{\"content\":\"joined\"}}]}\r\n\r\n".to_owned();
    let (url, _) = backend(
        "text/event-stream",
        vec![(0, split), (0, "data: [DONE]\n\n".to_owned())],
    )
    .await;
    let r = provider(url)
        .complete(
            &req(Some(progress(300, 150, 5000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        (r.message.content.as_deref(), r.model_id.as_deref()),
        (Some("joined"), Some("m"))
    );
    // An error event while it writes.
    let (url, _) = backend(
        "text/event-stream",
        vec![
            (0, token("a")),
            (0, event(r#"{"error":{"message":"oom"}}"#)),
        ],
    )
    .await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("failed while it wrote"), "{m}");
}

#[tokio::test]
async fn a_stream_larger_than_the_cap_is_refused() {
    // Events of 1 MiB each: they move the timers, so only the cap can end it.
    let big = || token(&"x".repeat(1 << 20));
    let (url, _) = backend("text/event-stream", (0..9).map(|_| (0, big())).collect()).await;
    match provider(url)
        .complete(
            &req(Some(progress(10_000, 10_000, 30_000))),
            &CancellationToken::new(),
        )
        .await
    {
        Err(ModelError::Provider(m)) => assert!(m.contains("exceeds"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Under the cap, the same events make an answer.
    let mut script: Vec<Step> = (0..7).map(|_| (0, big())).collect();
    script.push((0, "data: [DONE]\n\n".to_owned()));
    let (url, _) = backend("text/event-stream", script).await;
    let r = provider(url)
        .complete(
            &req(Some(progress(10_000, 10_000, 30_000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.message.content.map(|c| c.len()), Some(7 << 20));
}

/// A backend that frames its answer in chunks, as Ollama does, and either
/// ends the framing or closes in the middle of it.
async fn chunked(script: Vec<String>, end: bool) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _ = request(&mut s).await;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        s.write_all(head.as_bytes()).await.unwrap();
        for part in script {
            let framed = format!("{:x}\r\n{part}\r\n", part.len());
            s.write_all(framed.as_bytes()).await.unwrap();
        }
        if end {
            s.write_all(b"0\r\n\r\n").await.unwrap();
        }
    });
    url
}

#[tokio::test]
async fn a_chunked_stream_that_breaks_off_is_unavailable_and_one_that_ends_is_whole() {
    let url = chunked(vec![token("half")], false).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("broke off") || m.contains("ended before"), "{m}");
    let url = chunked(
        vec![token("whole"), finish(), "data: [DONE]\n\n".to_owned()],
        true,
    )
    .await;
    let r = provider(url)
        .complete(
            &req(Some(progress(300, 150, 5000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.message.content.as_deref(), Some("whole"));
}

/// How a backend that takes a request answers it instead of with HTTP.
#[derive(Clone, Copy, Debug)]
enum Instead {
    /// It closes without a word, as Ollama does when it is stopped while it
    /// reads the prompt.
    Closes,
    /// It resets the connection.
    Resets,
    /// It writes something that is not HTTP: a wrong port, not one going away.
    NotHttp,
}

async fn answers_with(instead: Instead) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let _ = request(&mut s).await;
            match instead {
                Instead::Closes => {}
                // A zero linger sends RST on drop and never blocks, which is
                // what the deprecation warns of.
                #[allow(deprecated)]
                Instead::Resets => s.set_linger(Some(Duration::ZERO)).unwrap(),
                Instead::NotHttp => {
                    let _ = s.write_all(b"not http\r\n\r\n").await;
                }
            }
        }
    });
    url
}

#[tokio::test]
async fn a_backend_that_hangs_up_before_it_answers_is_unavailable() {
    // r4 acceptance D2/D2b: this was `Provider`, so the job failed for good
    // instead of waiting for the backend to come back.
    let c = CancellationToken::new();
    for instead in [Instead::Closes, Instead::Resets] {
        let p = provider(answers_with(instead).await);
        for r in [req(None), req(Some(progress(1000, 1000, 5000)))] {
            let m = unavailable(p.complete(&r, &c).await).await;
            assert!(m.contains("error sending request"), "{instead:?}: {m}");
        }
        let models = p.models(&c).await;
        assert!(
            matches!(models, Err(ModelError::Unavailable(_))),
            "{instead:?}: {models:?}"
        );
    }
    // Control: an answer that is not HTTP is the backend's fault, not it going away.
    let p = provider(answers_with(Instead::NotHttp).await);
    for r in [req(None), req(Some(progress(1000, 1000, 5000)))] {
        let got = p.complete(&r, &c).await;
        assert!(matches!(got, Err(ModelError::Provider(_))), "{got:?}");
    }
}

#[tokio::test]
async fn a_whole_answer_that_breaks_off_is_unavailable_and_a_malformed_one_is_not() {
    /// A backend that answers 200 with `head` and `body`, then closes.
    async fn answers(head: &'static str, body: &'static str) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let _ = request(&mut s).await;
            let all =
                format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{head}\r\n{body}");
            s.write_all(all.as_bytes()).await.unwrap();
        });
        url
    }
    let c = CancellationToken::new();
    for (head, body) in [
        ("content-length: 100\r\n", r#"{"choices":"#),
        ("transfer-encoding: chunked\r\n", "b\r\n{\"choices\":\r\n"),
    ] {
        let url = answers(head, body).await;
        let m = unavailable(provider(url).complete(&req(None), &c).await).await;
        assert!(m.contains("broke off"), "{head}: {m}");
    }
    // Controls: the backend's fault, not it going away. A chunk size that is
    // no number, and an answer that ends whole but is no JSON.
    let url = answers("transfer-encoding: chunked\r\n", "ZZ\r\n{}\r\n0\r\n\r\n").await;
    let got = provider(url).complete(&req(None), &c).await;
    assert!(matches!(got, Err(ModelError::Provider(_))), "{got:?}");
    let (url, _) = backend("application/json", vec![(0, "not json".to_owned())]).await;
    let got = provider(url).complete(&req(None), &c).await;
    assert!(matches!(got, Err(ModelError::Provider(_))), "{got:?}");
}

#[tokio::test]
async fn an_error_answer_that_never_ends_is_cut_at_the_deadline() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _ = request(&mut s).await;
        let head = "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n5\r\n{\"err\r\n";
        s.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let started = Instant::now();
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 600))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("500"), "{m}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn the_formats_backends_write_are_read() {
    // `data:` with no space, a null content, and a character split across two writes.
    let bytes = "data:{\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":null}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"中文\"}}]}\n\ndata: [DONE]\n\n";
    let cut = bytes.find('中').unwrap() + 1;
    let (head, tail) = bytes.as_bytes().split_at(cut);
    let (head, tail) = (head.to_vec(), tail.to_vec());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _ = request(&mut s).await;
        s.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        s.write_all(&head).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.write_all(&tail).await.unwrap();
    });
    let r = provider(url)
        .complete(
            &req(Some(progress(1000, 1000, 5000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        (r.message.content.as_deref(), r.model_id.as_deref()),
        (Some("中文"), Some("m"))
    );
}

#[tokio::test]
async fn a_cancel_while_it_writes_ends_the_call_at_once() {
    let (url, _) = backend("text/event-stream", vec![(0, token("a")), (3000, finish())]).await;
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let started = Instant::now();
    let r = provider(url)
        .complete(&req(Some(progress(1000, 1000, 5000))), &cancel)
        .await;
    assert!(matches!(r, Err(ModelError::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn a_backend_that_ignores_stream_is_read_whole() {
    let body = r#"{"model":"whole-7b","choices":[{"message":{"role":"assistant","content":"all"}}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
    let (url, _) = backend("application/json", vec![(0, body.to_owned())]).await;
    let r = provider(url)
        .complete(
            &req(Some(progress(300, 150, 5000))),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        (r.message.content.as_deref(), r.model_id.as_deref()),
        (Some("all"), Some("whole-7b"))
    );
    // The same backend answering as an event stream, then hanging: the stall timer shows it was read as one.
    let (url, _) = backend("text/event-stream", vec![(0, token("a")), (3000, finish())]).await;
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(300, 150, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("no output for"), "{m}");
}

#[tokio::test]
async fn a_chunk_of_many_lines_is_read_in_one_pass() {
    // 2 MiB of blank lines in one frame, then the backend goes: read line by
    // line with a move of the rest each time, this alone took over 4s.
    let url = chunked(vec!["\n".repeat(2 << 20)], false).await;
    let started = Instant::now();
    let m = unavailable(
        provider(url)
            .complete(
                &req(Some(progress(5000, 5000, 5000))),
                &CancellationToken::new(),
            )
            .await,
    )
    .await;
    assert!(m.contains("broke off") || m.contains("ended before"), "{m}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_cancel_while_an_error_is_read_is_a_cancel() {
    // The error body stops on the cancel too; which one wins must not be luck.
    for _ in 0..20 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let _ = request(&mut s).await;
            let head = "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n5\r\n{\"err\r\n";
            s.write_all(head.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.cancel();
        });
        let r = provider(url)
            .complete(&req(Some(progress(1000, 1000, 5000))), &cancel)
            .await;
        assert!(matches!(r, Err(ModelError::Cancelled)), "{r:?}");
        server.abort();
    }
}

#[tokio::test]
async fn a_streamed_request_with_tools_is_refused_before_it_is_sent() {
    // Nothing listens there: a request that went out would be Unavailable.
    let mut r = req(Some(progress(1000, 1000, 5000)));
    r.tools = vec![crate::ToolSpec {
        name: "t".to_owned(),
        description: "d".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let got = provider("http://127.0.0.1:9".to_owned())
        .complete(&r, &CancellationToken::new())
        .await;
    assert!(
        matches!(&got, Err(ModelError::Provider(m)) if m.contains("tools")),
        "{got:?}"
    );
    r.tools.clear();
    let got = provider("http://127.0.0.1:9".to_owned())
        .complete(&r, &CancellationToken::new())
        .await;
    assert!(matches!(got, Err(ModelError::Unavailable(_))), "{got:?}");
}

#[tokio::test]
async fn a_streamed_request_carries_its_temperature() {
    // ME4-S2b: the temperature goes out with the stream keys, not instead of them.
    let script = vec![
        (0, token("x")),
        (0, finish()),
        (0, "data: [DONE]\n\n".to_owned()),
    ];
    let (url, asked) = backend("text/event-stream", script).await;
    let r = CompletionRequest {
        temperature: Some(0.0),
        ..req(Some(progress(1000, 1000, 5000)))
    };
    provider(url)
        .complete(&r, &CancellationToken::new())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&asked.await.unwrap()).unwrap();
    assert_eq!(
        (body["stream"].as_bool(), body["temperature"].as_f64()),
        (Some(true), Some(0.0))
    );
}

#[tokio::test]
async fn a_backend_that_hangs_up_while_a_large_prompt_is_written_is_unavailable() {
    // #1021 L1: the write meets the hang-up, as `BrokenPipe` or
    // `ConnectionReset`, whichever comes first: on macOS, 400 calls met 385
    // resets and 15 broken pipes, all through `hung_up`'s io branch and none
    // through a hyper predicate. That both kinds count is pinned by
    // `went_away`'s own test.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let _ = s.read(&mut [0u8; 4096]).await;
        }
    });
    let big = CompletionRequest {
        messages: vec![Msg::user("x".repeat(16 << 20))],
        ..req(None)
    };
    let streamed = CompletionRequest {
        // The write is what is under test, so the timers must not be able to
        // end the call first. A 16 MiB body is serialized and written inside
        // the first-chunk timer, which takes longer than a 1s limit in a debug
        // build (every run failed with `no output within 1s`, #1158). A minute
        // is out of reach for the hang-up, which is certain: the server has
        // already closed, so the write is reset.
        progress: Some(progress(60_000, 60_000, 120_000)),
        ..big.clone()
    };
    let p = provider(url);
    for r in [big, streamed] {
        let m = unavailable(p.complete(&r, &CancellationToken::new()).await).await;
        assert!(m.contains("error sending request"), "{m}");
    }
}
