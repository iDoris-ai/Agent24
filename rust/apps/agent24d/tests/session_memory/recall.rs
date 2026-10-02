use super::*;
use agent24_memory::{
    assertion::{AssertionStore, BeliefQuery},
    event::{Origin, Scope, Trust},
    writer::{Candidate, MemoryWriter},
};
use serde_json::json;

const RECALL_SESSION: &str = "m1-t07-recall-session";
const MODULE: &str = "recall_probe";

async fn add_fact(kv: &KvStore, owner: &str, id: &str, fact: &str) {
    kv.write_gate()
        .propose(vec![
            Candidate::new(
                id,
                Scope::owner(owner),
                "user",
                "said_to_remember",
                json!(fact),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            )
            .with_evidence(vec![format!("evidence-{id}")])
            .remember(),
        ])
        .await
        .unwrap();
}

fn recalled_text(messages: &[Msg]) -> Vec<&str> {
    messages
        .iter()
        .filter(|message| message.role == "system")
        .filter_map(|message| message.content.as_deref())
        .filter(|content| content.starts_with("你记得关于用户的这些事："))
        .collect()
}

#[tokio::test]
async fn full_question_recalls_only_personal_assertions_and_audits_injected_ids() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
    let personal = partition_key(&org, &SpaceId::personal(LOCAL_USER));
    let module = partition_key(&org, &SpaceId::module_private(MODULE));
    run(&state, "记住我对花生过敏").await;
    add_fact(&kv, &module, "peanut-module", "我对花生过敏（模块专属）").await;
    assert_eq!(
        kv.retriever()
            .search_any("过敏", &module, 5)
            .await
            .unwrap()
            .len(),
        1
    );
    let personal_rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&personal))
        .await
        .unwrap();
    assert_eq!(personal_rows.len(), 1);
    let personal_id = personal_rows[0].id.clone();
    ensure_session(&state, RECALL_SESSION).await;
    run_in_session(&state, RECALL_SESSION, "你好").await;
    let mut events = state.events.subscribe();

    let run = run_in_session(&state, RECALL_SESSION, "我对什么过敏？").await;

    let calls = provider.received.lock().unwrap();
    let messages = calls.last().expect("provider should receive the run");
    let recalled = recalled_text(messages);
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0], "你记得关于用户的这些事：\n- 我对花生过敏");
    assert_eq!(messages[0], Msg::system(recalled[0]));
    assert!(messages.contains(&Msg::user("你好")));
    drop(calls);

    let audits: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
        .map(|(_, body)| serde_json::to_value(body).unwrap())
        .filter(|body| body["type"] == "memory.recalled")
        .collect();
    assert_eq!(audits.len(), 1);
    let serialized = &audits[0];
    assert_eq!(serialized["payload"]["run_id"], run.id);
    assert_eq!(serialized["payload"]["ids"], json!([personal_id]));
}

#[tokio::test]
async fn unrelated_question_does_not_inject_memory() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
    let personal = partition_key(&org, &SpaceId::personal(LOCAL_USER));
    add_fact(&kv, &personal, "peanut-personal", "我对花生过敏").await;
    let mut events = state.events.subscribe();

    run(&state, "你能推荐一本历史书吗？").await;

    assert!(recalled_text(provider.received.lock().unwrap().last().unwrap()).is_empty());
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|(_, body)| { serde_json::to_value(body).unwrap()["type"] != "memory.recalled" })
    );
}

#[tokio::test]
async fn recall_budget_zero_one_item_and_default_top_five_match_audit_ids() {
    for mode in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
        let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
        let owner = partition_key(&org, &SpaceId::personal(LOCAL_USER));
        for (i, suffix) in ["甲", "乙", "丙", "丁", "戊", "己"].into_iter().enumerate() {
            add_fact(
                &kv,
                &owner,
                &format!("peanut-{i}"),
                &format!("我对花生过敏{suffix}"),
            )
            .await;
        }
        let prompt = "我对什么过敏？";
        let hits = kv.retriever().search_any(prompt, &owner, 5).await.unwrap();
        assert_eq!(hits.len(), 5);
        let top_one_budget = "你记得关于用户的这些事：\n- ".len()
            + hits[0].assertion.object.as_str().unwrap().len()
            + 4;
        let budget = match mode {
            0 => Some(0),
            1 => Some(top_one_budget),
            _ => None,
        };
        let provider = Arc::new(Provider::default());
        let state = app_with_recall_budget(kv, dir.path(), provider.clone(), budget).await;
        let mut events = state.events.subscribe();
        let run = run(&state, prompt).await;
        let calls = provider.received.lock().unwrap();
        let recalled = recalled_text(calls.last().unwrap());
        let expected = match mode {
            0 => vec![],
            1 => vec![hits[0].assertion.id.clone()],
            _ => hits.iter().map(|hit| hit.assertion.id.clone()).collect(),
        };
        assert_eq!(recalled.len(), usize::from(!expected.is_empty()));
        if !expected.is_empty() {
            let facts: Vec<_> = hits
                .iter()
                .filter(|hit| expected.contains(&hit.assertion.id))
                .map(|hit| hit.assertion.object.as_str().unwrap())
                .collect();
            let expected_text = format!("你记得关于用户的这些事：\n- {}", facts.join("\n- "));
            assert_eq!(recalled[0], expected_text);
        }
        drop(calls);
        let audits: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|(_, body)| serde_json::to_value(body).unwrap())
            .filter(|body| body["type"] == "memory.recalled")
            .collect();
        assert_eq!(audits.len(), usize::from(!expected.is_empty()));
        if let Some(audit) = audits.first() {
            assert_eq!(audit["payload"]["ids"], json!(expected));
            assert_eq!(audit["payload"]["run_id"], run.id);
        }
    }
}
