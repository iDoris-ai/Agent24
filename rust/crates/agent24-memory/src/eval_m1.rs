//! M1-T08 + M1-T07.2: a small, mostly-Chinese, hand-written recall-eval
//! baseline over [`crate::retriever::FtsRetriever::search_any`] — the
//! retriever M1's `recall` (M1-T07/T07.1) actually calls. This is NOT the
//! LongMemEval replay+condense harness in [`crate::eval`] (that one measures
//! a different pipeline: message history → condenser view).
//! `docs/research/MEMORY-STRATEGY.md` §4.1 T08 row: hand-written cases,
//! layered, plus distractor assertions.
//!
//! Review (#675) raised the corpus to production shape and added real gates
//! on top of the base "record only" spec:
//! - every fixture is written through [`crate::writer::WriteGate`] with
//!   subject `"user"` / predicate `"said_to_remember"` — the SAME shape
//!   `agent24-agent::retain::persist` uses in production (not the synthetic
//!   subject/predicate the first version of this file used), except the two
//!   supersede pairs, which need the raw [`crate::assertion::AssertionStore`]
//!   API because `WriteGate` has no supersede path yet (M1 scope, per the
//!   plan: "每次记住都是独立断言，撤回靠 T09");
//! - `forbidden_ids` is split into `security_forbidden_ids` (cross-owner,
//!   superseded, retracted, or unqualified — a leak here is a correctness
//!   bug, HARD GATE, must be 0) and `distractor_forbidden_ids` (same-owner
//!   adversarial text that merely shares vocabulary — a precision issue,
//!   record only);
//! - 6 English/neutral no-answer prompts were added specifically to prove
//!   the M1-T07.2 stopword/function-word filter (see `retriever.rs`); their
//!   injection rate is a HARD GATE (must be 0), while the original 3
//!   Chinese no-answer cases stay record-only (residual lexical imprecision
//!   un-fixed by this change is still possible there);
//! - Hit@5 over the original 14 answerable cases is a HARD GATE (>= 13/14).
//!
//! Review round 2 (#675) added 2 more cases, `cjk_safety_bigram`, kept OUT
//! of the 14-case Hit@5 ratio above (which is calibrated to that original
//! count) and instead given their OWN hard gate (both must hit): these
//! exist specifically to catch the regression `retriever.rs`'s CJK bigram
//! filter had at one point, where a real two-character word that merely
//! CONTAINS a pronoun/particle character as one syllable ("吗啡"/morphine
//! contains 吗) was silently dropped — a safety-relevant miss (a forgotten
//! allergy), not just a precision one.
//!
//! Everything else (Hit@1, All-evidence@5, distractor precision, the
//! original Chinese no-answer injection rate) stays record-only per the base
//! spec. Run: `cargo test -p agent24-memory eval_m1 -- --nocapture`.

#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::KvStore;
use crate::assertion::{Assertion, AssertionStore};
use crate::event::{Origin, Scope, Trust};
use crate::writer::{Candidate, MemoryWriter};
use serde_json::json;

const OWNER: &str = "m1-eval-owner";
const OTHER_OWNER: &str = "m1-eval-other-owner";
const TOP_K: usize = 5;
/// `forget`/supersede effective time: must be in the PAST relative to test
/// execution — `beliefs_as_of`/`search_any`'s active-row semantics are
/// bi-temporal, so a FUTURE timestamp would not have taken effect yet (a
/// trap this eval fell into once already, see `agent24-agent`'s recall.rs
/// M4 test for the same lesson).
const PAST: &str = "2000-01-01T00:00:00Z";

/// One hand-written case. `expected_ids` are the assertion id(s) that make up
/// the correct answer (empty for a no-answer case).
struct EvalCase {
    id: &'static str,
    category: &'static str,
    query: &'static str,
    expected_ids: &'static [&'static str],
    /// MUST never appear in the top-K hits for this query, corpus-wide: a
    /// cross-owner fact, a superseded/retracted id, or an unqualified
    /// candidate. A leak here is a correctness bug (hard gate).
    security_forbidden_ids: &'static [&'static str],
    /// Same-owner adversarial text that shares vocabulary with the query but
    /// answers nothing — a precision issue, not a safety one (record only).
    distractor_forbidden_ids: &'static [&'static str],
}

/// The distractor assertions MEMORY-STRATEGY §4.1 T08 requires (>=5), called
/// out by id so the eval can assert the corpus actually contains them.
const DISTRACTOR_IDS: &[&str] = &[
    "d1-colleague-allergy",
    "d2-friend-dog",
    "d3-cousin-job",
    "d4-neighbor-birthday",
    "d5-no-travel",
];

/// How one fixture row is written — see the module doc for why supersede
/// needs the raw ledger API instead of `WriteGate`.
enum Write {
    /// `WriteGate` with `Trust::UserSaid` + `remember()` → `Committed`
    /// (qualified, in default recall) — the production "记住…" path.
    Qualified,
    /// `WriteGate` with `Trust::UserSaid`, no `remember()` → `Held`
    /// (unqualified candidate, out of default recall by policy).
    Candidate,
    /// Raw `AssertionLedger::assert` with `supersedes` set to the given id,
    /// closing that id's `recorded_to` in the same write.
    Supersedes(&'static str),
}

struct Fixture {
    id: &'static str,
    owner: &'static str,
    object: &'static str,
    write: Write,
}

const fn f(id: &'static str, owner: &'static str, object: &'static str) -> Fixture {
    Fixture {
        id,
        owner,
        object,
        write: Write::Qualified,
    }
}

/// Every case's supporting fact, the lifecycle old/new rows, the source-trust
/// qualified/candidate pairs, the cross-owner isolation facts, and the
/// distractors — all under [`OWNER`] except the two isolation facts, which
/// live under [`OTHER_OWNER`] on purpose. `u3-rabbit` is retracted
/// separately in [`seed`] (retract is a distinct write, not a fixture row).
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
    Fixture { write: Write::Supersedes("u1-old"), ..f("u1-new", OWNER, "我现在住在杭州") },
    f("u2-old", OWNER, "我的手机号是13800001111"),
    Fixture { write: Write::Supersedes("u2-old"), ..f("u2-new", OWNER, "我的手机号是13900002222") },
    f("u3-rabbit", OWNER, "我养过一只兔子"),
    // 2 source trust: a qualified belief vs. an unqualified candidate that
    // must stay out of default recall (same `qualified` gate `search_any`'s
    // SQL already applies).
    f("st1-vege-qualified", OWNER, "我是素食主义者"),
    Fixture { write: Write::Candidate, ..f("st1-vege-candidate", OWNER, "可能是素食主义者") },
    f("st2-spicy-qualified", OWNER, "我不吃辣"),
    Fixture { write: Write::Candidate, ..f("st2-spicy-candidate", OWNER, "可能不吃辣") },
    // 2 owner isolation: these two facts live under a DIFFERENT owner and
    // must never surface for OWNER's queries.
    f("iso1-lawyer", OTHER_OWNER, "我的律师叫王芳"),
    f("iso2-passport", OTHER_OWNER, "我的护照号码是G12345678"),
    // Distractors: same owner, adversarially close in VOCABULARY to a real
    // case's query, but never the correct answer to anything above.
    f("d1-colleague-allergy", OWNER, "我同事对海鲜过敏"),
    f("d2-friend-dog", OWNER, "我朋友家的狗叫大黄"),
    f("d3-cousin-job", OWNER, "我表哥在字节跳动工作"),
    f("d4-neighbor-birthday", OWNER, "我邻居的生日是三月十六日"),
    f("d5-no-travel", OWNER, "我去年没去旅行"),
    // 2 CJK safety bigrams (review #675 round 2 M2): real two-character
    // words that merely CONTAIN a pronoun/particle character as a
    // syllable — must still be recalled.
    f("safety-morphine", OWNER, "我对吗啡过敏"),
    f("safety-bar", OWNER, "附近那家酒吧很吵"),
];

fn candidate(id: &str, owner: &str, object: &str) -> Candidate {
    Candidate::new(
        id,
        Scope::owner(owner),
        "user",
        "said_to_remember",
        json!(object),
        Origin {
            source: "eval-m1-fixture".to_owned(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![format!("ev-{id}")])
}

async fn seed(store: &KvStore) {
    let gate = store.write_gate();
    let ledger = store.assertions();
    for fx in FIXTURES {
        match fx.write {
            Write::Qualified => {
                gate.propose(vec![candidate(fx.id, fx.owner, fx.object).remember()])
                    .await
                    .unwrap();
            }
            Write::Candidate => {
                // No `.remember()`: `Trust::UserSaid` without it stays
                // `Held` by policy — exactly how a non-"记住…" statement
                // from the user is handled in production.
                gate.propose(vec![candidate(fx.id, fx.owner, fx.object)])
                    .await
                    .unwrap();
            }
            Write::Supersedes(old) => {
                let mut assertion = Assertion::new(
                    fx.id,
                    Scope::owner(fx.owner),
                    "user",
                    "said_to_remember",
                    json!(fx.object),
                    vec![format!("ev-{}", fx.id)],
                );
                assertion.supersedes = Some(old.to_owned());
                ledger.assert(&assertion).await.unwrap();
            }
        }
    }
    ledger
        .retract(&"u3-rabbit".to_owned(), OWNER, PAST)
        .await
        .unwrap();
}

#[rustfmt::skip]
fn cases() -> Vec<EvalCase> {
    vec![
        EvalCase { id: "f1-peanut", category: "single_fact", query: "我对什么过敏？", expected_ids: &["f1-peanut"], security_forbidden_ids: &[], distractor_forbidden_ids: &["d1-colleague-allergy"] },
        EvalCase { id: "f2-dog", category: "single_fact", query: "我家狗叫什么名字？", expected_ids: &["f2-dog"], security_forbidden_ids: &[], distractor_forbidden_ids: &["d2-friend-dog"] },
        EvalCase { id: "f3-job", category: "single_fact", query: "我在哪家公司工作？", expected_ids: &["f3-job"], security_forbidden_ids: &[], distractor_forbidden_ids: &["d3-cousin-job"] },
        EvalCase { id: "f4-color", category: "single_fact", query: "我最喜欢什么颜色？", expected_ids: &["f4-color"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "m1-pets", category: "cross_session_multi_fact", query: "我养了哪些宠物？", expected_ids: &["m1-cat", "m1-parrot"], security_forbidden_ids: &["u3-rabbit"], distractor_forbidden_ids: &[] },
        EvalCase { id: "m2-brother", category: "cross_session_multi_fact", query: "我哥哥的情况是什么？", expected_ids: &["m2-brother-name", "m2-brother-job"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "m3-travel", category: "cross_session_multi_fact", query: "我去年去了哪些城市旅行？", expected_ids: &["m3-tokyo", "m3-kyoto"], security_forbidden_ids: &[], distractor_forbidden_ids: &["d5-no-travel"] },
        EvalCase { id: "t1-meeting", category: "time_expression", query: "我下周三有什么安排？", expected_ids: &["t1-meeting"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "t2-birthday", category: "time_expression", query: "我的生日是哪天？", expected_ids: &["t2-birthday"], security_forbidden_ids: &[], distractor_forbidden_ids: &["d4-neighbor-birthday"] },
        EvalCase { id: "t3-deadline", category: "time_expression", query: "这个月底我要做什么？", expected_ids: &["t3-deadline"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "u1-address", category: "update_retract_lifecycle", query: "我现在住在哪里？", expected_ids: &["u1-new"], security_forbidden_ids: &["u1-old"], distractor_forbidden_ids: &[] },
        EvalCase { id: "u2-phone", category: "update_retract_lifecycle", query: "我的手机号是多少？", expected_ids: &["u2-new"], security_forbidden_ids: &["u2-old"], distractor_forbidden_ids: &[] },
        EvalCase { id: "u3-rabbit-retracted", category: "update_retract_lifecycle", query: "我养过兔子吗？", expected_ids: &[], security_forbidden_ids: &["u3-rabbit"], distractor_forbidden_ids: &[] },
        EvalCase { id: "n1-turtle", category: "no_answer_distractor", query: "我养过乌龟吗？", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "n2-paris", category: "no_answer_distractor", query: "我去过巴黎吗？", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "n3-car", category: "no_answer_distractor", query: "我的车是什么牌子？", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "st1-vege", category: "source_trust", query: "我是素食主义者吗？", expected_ids: &["st1-vege-qualified"], security_forbidden_ids: &["st1-vege-candidate"], distractor_forbidden_ids: &[] },
        EvalCase { id: "st2-spicy", category: "source_trust", query: "我吃辣吗？", expected_ids: &["st2-spicy-qualified"], security_forbidden_ids: &["st2-spicy-candidate"], distractor_forbidden_ids: &[] },
        EvalCase { id: "iso1-lawyer", category: "owner_isolation", query: "我的律师叫什么名字？", expected_ids: &[], security_forbidden_ids: &["iso1-lawyer"], distractor_forbidden_ids: &[] },
        EvalCase { id: "iso2-passport", category: "owner_isolation", query: "我的护照号码是多少？", expected_ids: &[], security_forbidden_ids: &["iso2-passport"], distractor_forbidden_ids: &[] },
        // M1-T07.2 (review #675): 6 English/neutral prompts that must inject
        // NOTHING — every fixture's subject/predicate is "user"/
        // "said_to_remember", so without filtering English stopwords + the
        // metadata words themselves, "to" (from "said_to_remember") or "my"/
        // "what" would spuriously OR-match every single fixture. Hard gate.
        EvalCase { id: "en1-python-loop", category: "no_answer_english_neutral", query: "how to write a for loop in python", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "en2-fix-bug", category: "no_answer_english_neutral", query: "fix the bug in my code", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "en3-capital", category: "no_answer_english_neutral", query: "what is the capital of France", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "en4-quantum", category: "no_answer_english_neutral", query: "解释一下量子计算", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "en5-dinner", category: "no_answer_english_neutral", query: "我在想晚饭吃什么", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "en6-proposal", category: "no_answer_english_neutral", query: "我对这个方案有意见", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        // M1-T07.2 round 2 (review #675 M2): must be recalled — regression
        // cases for the "contains a function character" over-filtering bug.
        EvalCase { id: "safety-morphine", category: "cjk_safety_bigram", query: "我对吗啡过敏吗？", expected_ids: &["safety-morphine"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "safety-bar", category: "cjk_safety_bigram", query: "附近有什么酒吧？", expected_ids: &["safety-bar"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
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
    no_answer_en: usize,
    no_answer_en_with_any_hit: usize,
    cases_with_distractor: usize,
    distractor_leaks: usize,
    cases_with_security_forbidden: usize,
    security_leaks: usize,
    cjk_safety_total: usize,
    cjk_safety_hits: usize,
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
        cases.len() >= 15,
        "M1-T08 asks for >=15 hand-written cases: got {}",
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
    println!("—— M1-T08 + T07.2 recall eval baseline (search_any, top_{TOP_K}) ——");
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
        let security_leaked: Vec<&&str> = case
            .security_forbidden_ids
            .iter()
            .filter(|forbidden| hit_ids.contains(&**forbidden))
            .collect();
        let distractor_leaked: Vec<&&str> = case
            .distractor_forbidden_ids
            .iter()
            .filter(|forbidden| hit_ids.contains(&**forbidden))
            .collect();

        if case.expected_ids.is_empty() {
            if case.category == "no_answer_english_neutral" {
                metrics.no_answer_en += 1;
                if !hit_ids.is_empty() {
                    metrics.no_answer_en_with_any_hit += 1;
                }
            } else {
                metrics.no_answer += 1;
                if !hit_ids.is_empty() {
                    metrics.no_answer_with_any_hit += 1;
                }
            }
        } else if case.category == "cjk_safety_bigram" {
            // Kept OUT of `answerable`/Hit@5 — that ratio is calibrated to
            // the ORIGINAL 14 cases (review round 1); these 2 get their own
            // gate instead (both must hit, see below).
            metrics.cjk_safety_total += 1;
            metrics.cjk_safety_hits += usize::from(hit_at_5);
        } else {
            metrics.answerable += 1;
            metrics.hit_at_1 += usize::from(hit_at_1);
            metrics.hit_at_5 += usize::from(hit_at_5);
            if case.expected_ids.len() > 1 {
                metrics.multi_evidence += 1;
                metrics.all_evidence_at_5 += usize::from(all_evidence);
            }
        }
        if !case.security_forbidden_ids.is_empty() {
            metrics.cases_with_security_forbidden += 1;
            metrics.security_leaks += usize::from(!security_leaked.is_empty());
        }
        if !case.distractor_forbidden_ids.is_empty() {
            metrics.cases_with_distractor += 1;
            metrics.distractor_leaks += usize::from(!distractor_leaked.is_empty());
        }

        println!(
            "[{cat}] {id}: query={q:?} hits={hits:?} expected={exp:?} hit@1={h1} hit@5={h5} security_leak={sl:?} distractor_leak={dl:?}",
            cat = case.category,
            id = case.id,
            q = case.query,
            hits = hit_ids,
            exp = case.expected_ids,
            h1 = hit_at_1,
            h5 = hit_at_5,
            sl = security_leaked,
            dl = distractor_leaked,
        );
    }

    println!("—— summary ——");
    println!(
        "Hit@1 (record only): {}/{} ({})",
        metrics.hit_at_1,
        metrics.answerable,
        pct(metrics.hit_at_1, metrics.answerable)
    );
    println!(
        "Hit@5 (HARD GATE >= 13/{}): {}/{} ({})",
        metrics.answerable,
        metrics.hit_at_5,
        metrics.answerable,
        pct(metrics.hit_at_5, metrics.answerable)
    );
    println!(
        "All-evidence@5 (record only): {}/{} ({})",
        metrics.all_evidence_at_5,
        metrics.multi_evidence,
        pct(metrics.all_evidence_at_5, metrics.multi_evidence)
    );
    println!(
        "irrelevant-injection rate, Chinese no-answer (record only): {}/{} ({})",
        metrics.no_answer_with_any_hit,
        metrics.no_answer,
        pct(metrics.no_answer_with_any_hit, metrics.no_answer)
    );
    println!(
        "irrelevant-injection rate, English/neutral no-answer (HARD GATE = 0): {}/{} ({})",
        metrics.no_answer_en_with_any_hit,
        metrics.no_answer_en,
        pct(metrics.no_answer_en_with_any_hit, metrics.no_answer_en)
    );
    println!(
        "distractor-precision leak rate (record only): {}/{} ({})",
        metrics.distractor_leaks,
        metrics.cases_with_distractor,
        pct(metrics.distractor_leaks, metrics.cases_with_distractor)
    );
    println!(
        "security-forbidden-ID leak rate (HARD GATE = 0): {}/{} ({})",
        metrics.security_leaks,
        metrics.cases_with_security_forbidden,
        pct(
            metrics.security_leaks,
            metrics.cases_with_security_forbidden
        )
    );
    println!(
        "CJK safety-bigram Hit@5 (HARD GATE = 100%): {}/{} ({})",
        metrics.cjk_safety_hits,
        metrics.cjk_safety_total,
        pct(metrics.cjk_safety_hits, metrics.cjk_safety_total)
    );

    assert_eq!(
        metrics.security_leaks, 0,
        "security leak (cross-owner / superseded / retracted / unqualified) must be zero"
    );
    assert_eq!(
        metrics.no_answer_en_with_any_hit, 0,
        "an English/neutral no-answer prompt must never inject a memory (M1-T07.2)"
    );
    assert!(
        metrics.hit_at_5 * 14 >= 13 * metrics.answerable,
        "Hit@5 must be >= 13/14 of the {} answerable cases: got {}/{}",
        metrics.answerable,
        metrics.hit_at_5,
        metrics.answerable
    );
    assert_eq!(
        metrics.cjk_safety_hits, metrics.cjk_safety_total,
        "every CJK safety-bigram case (a real word that merely contains a \
         pronoun/particle character) must be recalled (M1-T07.2 round 2 M2)"
    );
}
