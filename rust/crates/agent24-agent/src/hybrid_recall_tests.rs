// VR-3: recall through the hybrid retriever (FTS + vector) in the agent loop.
use super::*;
use agent24_memory::vector::{Embedder, Embedding};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Shares no token with `TEAL` ("my favourite colour is teal"), so FTS finds
/// nothing; the embedder puts both on one axis.
const HUE_PROMPT: &str = "which hue do I like best";
const OWNER: &str = "test-owner";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ok,
    Fail,
    Hang,
}

#[derive(Clone)]
struct HueEmbedder {
    query_calls: Arc<AtomicUsize>,
    mode: Mode,
}

impl HueEmbedder {
    fn new(mode: Mode) -> Self {
        Self {
            query_calls: Arc::new(AtomicUsize::new(0)),
            mode,
        }
    }
    fn calls(&self) -> usize {
        self.query_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for HueEmbedder {
    fn model_id(&self) -> &str {
        "hue"
    }
    fn revision(&self) -> &str {
        "v1"
    }
    async fn embed(&self, text: &str) -> agent24_memory::Result<Embedding> {
        // Facts are embedded as "user said_to_remember <object>" by reindex.
        if !text.starts_with("user said_to_remember") {
            self.query_calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                Mode::Ok => {}
                Mode::Fail => return Err(agent24_memory::MemoryError::Embedder("down".into())),
                Mode::Hang => std::future::pending::<()>().await,
            }
        }
        let vector = if text.contains("teal") || text.contains("hue") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        };
        Ok(Embedding {
            model_id: "hue".into(),
            revision: "v1".into(),
            dims: 2,
            normalized: false,
            vector,
        })
    }
}

async fn memory_over(kv: &KvStore, embedder: Option<HybridWiring>) -> SessionMemory {
    let memory = SessionMemory::new(kv.clone(), Arc::new(UnusedSummarizer)).with_owner(OWNER.into());
    match embedder {
        None => memory,
        Some(HybridWiring::Default(e)) => memory.with_hybrid(
            agent24_memory::test_hooks::hybrid_retriever(kv, Arc::new(e)),
        ),
        Some(HybridWiring::ShortTimeout(e)) => memory.with_hybrid(
            agent24_memory::test_hooks::hybrid_retriever(kv, Arc::new(e))
                .with_timing(Duration::from_millis(50), Duration::from_secs(30)),
        ),
    }
}

enum HybridWiring {
    Default(HueEmbedder),
    ShortTimeout(HueEmbedder),
}

async fn embed_all(kv: &KvStore, embedder: &HueEmbedder) {
    kv.vector_retriever(embedder.clone())
        .reindex(OWNER)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_paraphrase_with_no_keyword_hit_is_recalled_through_vectors() {
    let kv = KvStore::open_memory().await.unwrap();
    seed_memory(&kv, "a-teal", Some(agent24_store::SourceClassification::Ordinary)).await;
    let embedder = HueEmbedder::new(Mode::Ok);
    embed_all(&kv, &embedder).await;
    let plain = memory_over(&kv, None).await;
    assert!(plain.recall(HUE_PROMPT).await.unwrap().is_none(), "premise: FTS alone finds nothing");
    let hybrid = memory_over(&kv, Some(HybridWiring::Default(embedder.clone()))).await;
    let (msg, ids) = hybrid.recall(HUE_PROMPT).await.unwrap().expect("recalled");
    assert!(msg.content.unwrap().contains(TEAL));
    assert_eq!(ids.len(), 1);
    assert_eq!(embedder.calls(), 1);
}

#[tokio::test]
async fn an_unknown_fact_found_only_by_vectors_stays_unknown_and_blocks_egress() {
    use agent24_store::SourceClassification::Unknown;
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let store = Store::open_memory().await.unwrap();
    let kv = KvStore::open_memory().await.unwrap();
    seed_memory(&kv, "a-teal", None).await; // legacy NULL source_ref: Unknown
    let embedder = HueEmbedder::new(Mode::Ok);
    embed_all(&kv, &embedder).await;
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider.clone(), Tier::Local)])),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingSink(StdMutex::new(vec![]))),
        CancellationToken::new(),
        Some(memory_over(&kv, Some(HybridWiring::Default(embedder.clone()))).await),
    );
    seed_session(&store, "sess_vec").await;
    let run = run_in_session(&manager, &store, "sess_vec", HUE_PROMPT).await;
    let seen = provider.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .flatten()
            .any(|m| m.content.as_deref().is_some_and(|c| c.contains(TEAL))),
        "the fact must be recalled via the vector route"
    );
    assert_eq!(embedder.calls(), 1);
    let snapshot = store.run_policy_snapshot(&run.id).await.unwrap();
    let tag = snapshot
        .sources
        .iter()
        .find(|s| s.source_id == "memory_assertion:a-teal")
        .unwrap_or_else(|| panic!("no recall tag in {:?}", snapshot.sources));
    assert_eq!(tag.kind, agent24_store::SourceKind::MemoryRecall);
    assert_eq!(tag.classification, Unknown);
    assert!(!process_egress_allowed_in(&manager, &run).await);
}

#[tokio::test]
async fn embedder_error_timeout_and_empty_index_recall_identically_to_pure_fts() {
    let prompt = "what colour is teal";
    for (label, mode, indexed) in [
        ("error", Mode::Fail, true),
        ("timeout", Mode::Hang, true),
        ("empty index", Mode::Ok, false),
    ] {
        let kv = KvStore::open_memory().await.unwrap();
        seed_memory(&kv, "a-teal", Some(agent24_store::SourceClassification::Ordinary)).await;
        let embedder = HueEmbedder::new(mode);
        if indexed {
            embed_all(&kv, &embedder).await;
        }
        let expected = memory_over(&kv, None).await.recall(prompt).await.unwrap();
        assert!(expected.is_some(), "{label}: premise: FTS recalls this");
        let wiring = if mode == Mode::Hang {
            HybridWiring::ShortTimeout(embedder.clone())
        } else {
            HybridWiring::Default(embedder.clone())
        };
        let got = memory_over(&kv, Some(wiring)).await.recall(prompt).await.unwrap();
        assert_eq!(got, expected, "{label}: must be byte-identical to pure FTS");
        assert_eq!(embedder.calls(), usize::from(indexed), "{label}");
    }
}

#[tokio::test]
async fn paused_personal_memory_never_calls_the_embedder() {
    let kv = KvStore::open_memory().await.unwrap();
    seed_memory(&kv, "a-teal", Some(agent24_store::SourceClassification::Ordinary)).await;
    let embedder = HueEmbedder::new(Mode::Ok);
    embed_all(&kv, &embedder).await;
    let memory = memory_over(&kv, Some(HybridWiring::Default(embedder.clone()))).await;
    // Positive control: enabled, the same recall does call it.
    assert!(memory.recall(HUE_PROMPT).await.unwrap().is_some());
    assert_eq!(embedder.calls(), 1);
    kv.set_memory_enabled(OWNER, false).await.unwrap();
    assert!(memory.recall(HUE_PROMPT).await.unwrap().is_none());
    assert_eq!(embedder.calls(), 1, "paused: no further embedder calls");
}

#[tokio::test]
async fn another_owners_fact_is_not_recalled_through_vectors() {
    use agent24_memory::event::Scope;
    use agent24_memory::writer::{Candidate, MemoryWriter};
    let kv = KvStore::open_memory().await.unwrap();
    kv.write_gate()
        .propose(vec![
            Candidate::new(
                "theirs",
                Scope::owner("someone-else"),
                "user",
                "said_to_remember",
                serde_json::json!(TEAL),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            )
            .with_evidence(vec!["ev".into()])
            .remember(),
        ])
        .await
        .unwrap();
    seed_memory(&kv, "mine", Some(agent24_store::SourceClassification::Ordinary)).await;
    // Make "mine" not match the hue axis, so only "theirs" could.
    sqlx::query("UPDATE mem_assertions SET object = '\"my cat is Tom\"' WHERE id = 'mine'")
        .execute(agent24_memory::test_hooks::pool(&kv))
        .await
        .unwrap();
    let embedder = HueEmbedder::new(Mode::Ok);
    embed_all(&kv, &embedder).await;
    kv.vector_retriever(embedder.clone())
        .reindex("someone-else")
        .await
        .unwrap();
    let memory = memory_over(&kv, Some(HybridWiring::Default(embedder.clone()))).await;
    assert!(memory.recall(HUE_PROMPT).await.unwrap().is_none());
}
