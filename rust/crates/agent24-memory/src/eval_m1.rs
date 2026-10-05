//! M1-T08: a small, mostly-Chinese, hand-written recall-eval baseline over
//! [`crate::retriever::FtsRetriever::search_any`] — the retriever M1's `recall`
//! (M1-T07/T07.1) actually calls. This is NOT the LongMemEval replay+condense
//! harness in [`crate::eval`] (that one measures a different pipeline: message
//! history → condenser view). `docs/research/MEMORY-STRATEGY.md` §4.1 T08 row:
//! ≤20 hand-written cases, layered, plus ≥5 distractor assertions; **record
//! only, no gate**.
//!
//! Layering (exactly 20 cases):
//! - 4 single-fact
//! - 3 cross-session/multi-fact (2+ assertions make up one correct answer)
//! - 3 time expressions
//! - 3 update/retract lifecycle (hard owner-isolation/retract GATES live in
//!   T07/T09/T10's own tests — this module only records the recall metrics)
//! - 3 no-answer/literal distractors
//! - 2 source trust (qualified vs. an unqualified candidate)
//! - 2 owner isolation
//!
//! Run: `cargo test -p agent24-memory eval_m1 -- --nocapture`.

#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::KvStore;
use crate::assertion::{Assertion, AssertionStore};
use crate::event::Scope;
use serde_json::json;

const OWNER: &str = "m1-eval-owner";
const OTHER_OWNER: &str = "m1-eval-other-owner";
const TOP_K: usize = 5;

/// One hand-written case. `expected_ids` are the assertion id(s) that make up
/// the correct answer (empty for a no-answer case). `forbidden_ids` are ids
/// that must never appear among the top-K hits for THIS query — a cross-owner
/// fact, a superseded/retracted id, or an adversarial same-owner distractor
/// that shares vocabulary with the query but answers nothing above.
struct EvalCase {
    id: &'static str,
    category: &'static str,
    query: &'static str,
    expected_ids: &'static [&'static str],
    forbidden_ids: &'static [&'static str],
}

/// The ≥5 distractor assertions MEMORY-STRATEGY §4.1 T08 requires, called out
/// by id so the eval can assert the corpus actually contains them.
const DISTRACTOR_IDS: &[&str] = &[
    "d1-colleague-allergy",
    "d2-friend-dog",
    "d3-cousin-job",
    "d4-neighbor-birthday",
    "d5-no-travel",
];

fn a(id: &str, owner: &str, object: &str) -> Assertion {
    Assertion::new(
        id,
        Scope::owner(owner),
        "user",
        "said",
        json!(object),
        vec![format!("ev-{id}")],
    )
}

/// One row of the fixture table [`FIXTURES`] seeds. `qualified=false` makes an
/// unconfirmed candidate (source-trust pair); `supersedes` closes that id's
/// `recorded_to` in the SAME write (update lifecycle pair).
struct Fixture {
    id: &'static str,
    owner: &'static str,
    object: &'static str,
    qualified: bool,
    supersedes: Option<&'static str>,
}

const fn f(id: &'static str, owner: &'static str, object: &'static str) -> Fixture {
    Fixture {
        id,
        owner,
        object,
        qualified: true,
        supersedes: None,
    }
}

/// Every case's supporting fact, the lifecycle old/new rows, the source-trust
/// qualified/candidate pairs, the cross-owner isolation facts, and the
/// distractors — all under [`OWNER`] except the two isolation facts, which
/// live under [`OTHER_OWNER`] on purpose. `u3-rabbit` is retracted separately
/// in [`seed`] (retract is a distinct write, not a fixture row).
#[rustfmt::skip]
const FIXTURES: &[Fixture] = &[
    // 4 single-fact.
    f("f1-peanut", OWNER, "我对花生过敏"),
    f("f2-dog", OWNER, "我家的狗叫阿黄"),
    f("f3-job", OWNER, "我在字节跳动工作"),
    f("f4-color", OWNER, "我最喜欢的颜色是蓝色"),
    // 3 cross-session/multi-fact: each answer needs 2 assertions together.
    f("m1-cat", OWNER, "我养了一只猫叫咪咪"),
    f("m1-parrot", OWNER, "我还养了一只鹦鹉叫小绿"),
    f("m2-brother-name", OWNER, "我哥哥叫李明"),
    f("m2-brother-job", OWNER, "我哥哥在上海工作"),
    f("m3-tokyo", OWNER, "我去年去过东京"),
    f("m3-kyoto", OWNER, "我去年还去过京都"),
    // 3 time expressions.
    f("t1-meeting", OWNER, "下周三我要和产品团队开会"),
    f("t2-birthday", OWNER, "我的生日是三月十五日"),
    f("t3-deadline", OWNER, "这个月月底要提交项目报告"),
    // 3 update/retract lifecycle: old row superseded or retracted, never
    // leaking into recall even though it is lexically close to the query.
    f("u1-old", OWNER, "我以前住在北京"),
    Fixture { supersedes: Some("u1-old"), ..f("u1-new", OWNER, "我现在住在杭州") },
    f("u2-old", OWNER, "我的手机号是13800001111"),
    Fixture { supersedes: Some("u2-old"), ..f("u2-new", OWNER, "我的手机号是13900002222") },
    f("u3-rabbit", OWNER, "我养过一只兔子"),
    // 2 source trust: a qualified belief vs. an unqualified candidate that
    // must stay out of default recall (same `qualified` gate `search_any`'s
    // SQL already applies).
    f("st1-vege-qualified", OWNER, "我是素食主义者"),
    Fixture { qualified: false, ..f("st1-vege-candidate", OWNER, "可能是素食主义者") },
    f("st2-spicy-qualified", OWNER, "我不吃辣"),
    Fixture { qualified: false, ..f("st2-spicy-candidate", OWNER, "可能不吃辣") },
    // 2 owner isolation: these two facts live under a DIFFERENT owner and
    // must never surface for OWNER's queries.
    f("iso1-lawyer", OTHER_OWNER, "我的律师叫王芳"),
    f("iso2-passport", OTHER_OWNER, "我的护照号码是G12345678"),
    // >=5 distractors: same owner, adversarially close in VOCABULARY to a
    // real case's query, but never the correct answer to anything above.
    f("d1-colleague-allergy", OWNER, "我同事对海鲜过敏"),
    f("d2-friend-dog", OWNER, "我朋友家的狗叫大黄"),
    f("d3-cousin-job", OWNER, "我表哥在字节跳动工作"),
    f("d4-neighbor-birthday", OWNER, "我邻居的生日是三月十六日"),
    f("d5-no-travel", OWNER, "我去年没去旅行"),
];

async fn seed(store: &KvStore) {
    let l = store.assertions();
    for fixture in FIXTURES {
        let mut assertion = a(fixture.id, fixture.owner, fixture.object);
        assertion.qualified = fixture.qualified;
        assertion.supersedes = fixture.supersedes.map(str::to_owned);
        l.assert(&assertion).await.unwrap();
    }
    l.retract(&"u3-rabbit".to_owned(), OWNER, "2030-01-01T00:00:00Z")
        .await
        .unwrap();
}

#[rustfmt::skip]
fn cases() -> Vec<EvalCase> {
    vec![
        EvalCase { id: "f1-peanut", category: "single_fact", query: "我对什么过敏？", expected_ids: &["f1-peanut"], forbidden_ids: &["d1-colleague-allergy"] },
        EvalCase { id: "f2-dog", category: "single_fact", query: "我家狗叫什么名字？", expected_ids: &["f2-dog"], forbidden_ids: &["d2-friend-dog"] },
        EvalCase { id: "f3-job", category: "single_fact", query: "我在哪家公司工作？", expected_ids: &["f3-job"], forbidden_ids: &["d3-cousin-job"] },
        EvalCase { id: "f4-color", category: "single_fact", query: "我最喜欢什么颜色？", expected_ids: &["f4-color"], forbidden_ids: &[] },
        EvalCase { id: "m1-pets", category: "cross_session_multi_fact", query: "我养了哪些宠物？", expected_ids: &["m1-cat", "m1-parrot"], forbidden_ids: &["u3-rabbit"] },
        EvalCase { id: "m2-brother", category: "cross_session_multi_fact", query: "我哥哥的情况是什么？", expected_ids: &["m2-brother-name", "m2-brother-job"], forbidden_ids: &[] },
        EvalCase { id: "m3-travel", category: "cross_session_multi_fact", query: "我去年去了哪些城市旅行？", expected_ids: &["m3-tokyo", "m3-kyoto"], forbidden_ids: &["d5-no-travel"] },
        EvalCase { id: "t1-meeting", category: "time_expression", query: "我下周三有什么安排？", expected_ids: &["t1-meeting"], forbidden_ids: &[] },
        EvalCase { id: "t2-birthday", category: "time_expression", query: "我的生日是哪天？", expected_ids: &["t2-birthday"], forbidden_ids: &["d4-neighbor-birthday"] },
        EvalCase { id: "t3-deadline", category: "time_expression", query: "这个月底我要做什么？", expected_ids: &["t3-deadline"], forbidden_ids: &[] },
        EvalCase { id: "u1-address", category: "update_retract_lifecycle", query: "我现在住在哪里？", expected_ids: &["u1-new"], forbidden_ids: &["u1-old"] },
        EvalCase { id: "u2-phone", category: "update_retract_lifecycle", query: "我的手机号是多少？", expected_ids: &["u2-new"], forbidden_ids: &["u2-old"] },
        EvalCase { id: "u3-rabbit-retracted", category: "update_retract_lifecycle", query: "我养过兔子吗？", expected_ids: &[], forbidden_ids: &["u3-rabbit"] },
        EvalCase { id: "n1-turtle", category: "no_answer_distractor", query: "我养过乌龟吗？", expected_ids: &[], forbidden_ids: &[] },
        EvalCase { id: "n2-paris", category: "no_answer_distractor", query: "我去过巴黎吗？", expected_ids: &[], forbidden_ids: &[] },
        EvalCase { id: "n3-car", category: "no_answer_distractor", query: "我的车是什么牌子？", expected_ids: &[], forbidden_ids: &[] },
        EvalCase { id: "st1-vege", category: "source_trust", query: "我是素食主义者吗？", expected_ids: &["st1-vege-qualified"], forbidden_ids: &["st1-vege-candidate"] },
        EvalCase { id: "st2-spicy", category: "source_trust", query: "我吃辣吗？", expected_ids: &["st2-spicy-qualified"], forbidden_ids: &["st2-spicy-candidate"] },
        EvalCase { id: "iso1-lawyer", category: "owner_isolation", query: "我的律师叫什么名字？", expected_ids: &[], forbidden_ids: &["iso1-lawyer"] },
        EvalCase { id: "iso2-passport", category: "owner_isolation", query: "我的护照号码是多少？", expected_ids: &[], forbidden_ids: &["iso2-passport"] },
    ]
}

#[derive(Default)]
struct Metrics {
    answerable: usize,
    hit_at_1: usize,
    hit_at_5: usize,
    multi_evidence: usize,
    all_evidence_at_5: usize,
    no_answer: usize,
    no_answer_with_any_hit: usize,
    cases_with_forbidden: usize,
    forbidden_leaks: usize,
}

fn pct(n: usize, of: usize) -> String {
    if of == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / of as f64)
    }
}

#[tokio::test]
async fn eval_m1() {
    let cases = cases();
    assert!(
        (15..=20).contains(&cases.len()),
        "M1-T08 caps the eval corpus at <=20 cases and asks for >=15: got {}",
        cases.len()
    );
    assert!(
        DISTRACTOR_IDS.len() >= 5,
        "M1-T08 asks for >=5 distractor assertions"
    );

    let store = KvStore::open_memory().await.unwrap();
    seed(&store).await;
    let retriever = store.retriever();

    let mut metrics = Metrics::default();
    println!("—— M1-T08 recall eval baseline (search_any, top_{TOP_K}) ——");
    for case in &cases {
        let hits = retriever
            .search_any(case.query, OWNER, TOP_K)
            .await
            .unwrap();
        let hit_ids: Vec<&str> = hits.iter().map(|h| h.assertion.id.as_str()).collect();

        let hit_at_1 = hit_ids
            .first()
            .is_some_and(|id| case.expected_ids.contains(id));
        let hit_at_5 = hit_ids.iter().any(|id| case.expected_ids.contains(id));
        let all_evidence = !case.expected_ids.is_empty()
            && case
                .expected_ids
                .iter()
                .all(|expected| hit_ids.contains(expected));
        let leaked: Vec<&&str> = case
            .forbidden_ids
            .iter()
            .filter(|forbidden| hit_ids.contains(&**forbidden))
            .collect();

        if case.expected_ids.is_empty() {
            metrics.no_answer += 1;
            if !hit_ids.is_empty() {
                metrics.no_answer_with_any_hit += 1;
            }
        } else {
            metrics.answerable += 1;
            metrics.hit_at_1 += usize::from(hit_at_1);
            metrics.hit_at_5 += usize::from(hit_at_5);
            if case.expected_ids.len() > 1 {
                metrics.multi_evidence += 1;
                metrics.all_evidence_at_5 += usize::from(all_evidence);
            }
        }
        if !case.forbidden_ids.is_empty() {
            metrics.cases_with_forbidden += 1;
            metrics.forbidden_leaks += usize::from(!leaked.is_empty());
        }

        println!(
            "[{cat}] {id}: query={q:?} hits={hits:?} expected={exp:?} hit@1={h1} hit@5={h5} leaked_forbidden={leaked:?}",
            cat = case.category,
            id = case.id,
            q = case.query,
            hits = hit_ids,
            exp = case.expected_ids,
            h1 = hit_at_1,
            h5 = hit_at_5,
            leaked = leaked,
        );
    }

    println!("—— summary (record only, no gate) ——");
    println!(
        "Hit@1: {}/{} ({})",
        metrics.hit_at_1,
        metrics.answerable,
        pct(metrics.hit_at_1, metrics.answerable)
    );
    println!(
        "Hit@5: {}/{} ({})",
        metrics.hit_at_5,
        metrics.answerable,
        pct(metrics.hit_at_5, metrics.answerable)
    );
    println!(
        "All-evidence@5: {}/{} ({})",
        metrics.all_evidence_at_5,
        metrics.multi_evidence,
        pct(metrics.all_evidence_at_5, metrics.multi_evidence)
    );
    println!(
        "irrelevant-injection rate (no-answer cases returning >=1 hit): {}/{} ({})",
        metrics.no_answer_with_any_hit,
        metrics.no_answer,
        pct(metrics.no_answer_with_any_hit, metrics.no_answer)
    );
    println!(
        "forbidden-ID leak rate: {}/{} ({})",
        metrics.forbidden_leaks,
        metrics.cases_with_forbidden,
        pct(metrics.forbidden_leaks, metrics.cases_with_forbidden)
    );
}
