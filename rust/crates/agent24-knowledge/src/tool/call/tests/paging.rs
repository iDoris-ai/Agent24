//! A citation read cut by `max_bytes`, followed page by page.

use super::*;

/// Answers the i-th read with `pages[i]`, all for the pinned chunk read.
struct Pages {
    pages: Vec<Result<ReadResponse, Failure>>,
    asked: Mutex<Vec<ReadQuery>>,
}

impl Pages {
    fn new(pages: Vec<Result<ReadResponse, Failure>>) -> Self {
        Self {
            pages,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn cursors(&self) -> Vec<Option<String>> {
        let asked = self.asked.lock().unwrap();
        asked.iter().map(|q| q.cursor.clone()).collect()
    }
}

#[async_trait]
impl KnowledgeBackend for Pages {
    async fn search(
        &self,
        _: &str,
        _: SearchQuery,
    ) -> Result<(SearchRequest, SearchResponse), Failure> {
        panic!("no search here")
    }

    async fn read(
        &self,
        _: &str,
        query: ReadQuery,
    ) -> Result<(ReadRequest, ReadResponse), Failure> {
        let mut asked = self.asked.lock().unwrap();
        let page = self.pages[asked.len()].clone();
        asked.push(query);
        let (request, _): (ReadRequest, ReadResponse) = pair("read.chunk.json");
        page.map(|response| (request, response))
    }
}

/// A page of `chk-71` with `text`, cut with `next` as its cursor or the last.
fn page(text: &str, next: Option<&str>) -> ReadResponse {
    let mut v = fixture("read.chunk.json")["response"]["body"].clone();
    v["segments"][0]["text"] = json!(text);
    v["truncated"] = json!(next.is_some());
    v["next_cursor"] = json!(next);
    serde_json::from_value(v).unwrap()
}

fn cite() -> Map<String, Value> {
    input(json!({ "knowledge_id": "doc-7", "chunk_id": "chk-71", "content_tag": "ct-chk-71" }))
}

fn rendered(backend: &Pages, args: Map<String, Value>) -> Value {
    serde_json::from_str(&block(read(backend, "run-1", &args)).unwrap()).unwrap()
}

/// The first page stands as current text with its cursor.
fn assert_first_page(out: &Value) {
    assert_eq!(out["evidence"][0]["use"], "current_text");
    assert_eq!(out["evidence"][0]["text"], "一");
    assert_eq!(out["next_cursor"], "cur-1");
    assert_eq!(out["truncated"], true);
}

#[test]
fn a_cut_chunk_is_read_to_its_end_and_cited_whole() {
    let backend = Pages::new(vec![
        Ok(page("一", Some("cur-1"))),
        Ok(page("二", Some("cur-2"))),
        Ok(page("三", None)),
    ]);
    let out = rendered(&backend, cite());
    assert_eq!(out["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(out["evidence"][0]["use"], "citation");
    assert_eq!(out["evidence"][0]["text"], "一二三");
    assert_eq!(out["truncated"], false);
    assert!(out["next_cursor"].is_null());
    // Each page its own request, for the same chunk and tag.
    assert_eq!(
        backend.cursors(),
        [None, Some("cur-1".into()), Some("cur-2".into())]
    );
    for q in backend.asked.lock().unwrap().iter() {
        assert_eq!(q.chunk_id.as_deref(), Some("chk-71"));
        assert_eq!(q.content_tag.as_deref(), Some("ct-chk-71"));
    }
}

#[test]
fn a_page_of_another_text_leaves_the_first_page_as_current_text() {
    let mut other_tag = page("二", None);
    other_tag.segments[0].content_tag = "ct-chk-71-v2".into();
    let mut other_chunk = page("二", None);
    other_chunk.segments[0].chunk_id = "chk-72".into();
    let mut other_doc = page("二", None);
    other_doc.knowledge_id = "doc-8".into();
    let mut two = page("二", None);
    two.segments.push(two.segments[0].clone());
    for second in [other_tag, other_chunk, other_doc, two] {
        let backend = Pages::new(vec![Ok(page("一", Some("cur-1"))), Ok(second)]);
        assert_first_page(&rendered(&backend, cite()));
    }
}

#[test]
#[allow(clippy::result_large_err)]
fn past_the_page_limit_the_first_page_stands() {
    let pages = (0..PAGES_MAX)
        .map(|i| Ok(page(if i == 0 { "一" } else { "又" }, Some("cur-1"))))
        .collect();
    let backend = Pages::new(pages);
    let out = rendered(&backend, cite());
    assert_eq!(backend.cursors().len(), PAGES_MAX);
    assert_first_page(&out);
}

#[test]
fn a_page_refused_leaves_the_first_page_and_a_cancelled_one_cancels_the_call() {
    let backend = Pages::new(vec![
        Ok(page("一", Some("cur-1"))),
        Err(envelope("read.stale-chunk-cursor.json")),
    ]);
    assert_first_page(&rendered(&backend, cite()));

    let local = fixture("search.cancelled.json")["local"]["result"].clone();
    let backend = Pages::new(vec![
        Ok(page("一", Some("cur-1"))),
        Err(Failure::Cancelled(serde_json::from_value(local).unwrap())),
    ]);
    assert_eq!(
        block(read(&backend, "run-1", &cite())),
        Err(Refused::Cancelled)
    );
}

#[test]
fn only_a_citation_read_from_its_first_page_reads_on() {
    let no_tag = input(json!({ "knowledge_id": "doc-7", "chunk_id": "chk-71" }));
    let mut from_cursor = cite();
    from_cursor.insert("cursor".into(), json!("cur-1"));
    for args in [no_tag, from_cursor] {
        let backend = Pages::new(vec![Ok(page("一", Some("cur-1")))]);
        assert_first_page(&rendered(&backend, args));
        assert_eq!(backend.cursors().len(), 1);
    }
}
