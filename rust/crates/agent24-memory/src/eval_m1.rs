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
//!
//! VR-5 adds 20 Chinese paraphrase cases (a remembered sentence, then a
//! question that does not share a content bigram with it) and 4 no-answer
//! prompts. CI still embeds with [`NgramEmbedder`]. On that path the Hit@5
//! gate is the original 13/14 ratio applied to the answerable set **including
//! the paraphrases**, and the new no-answer prompts must inject nothing.
//! Pure FTS cannot see those paraphrases, so `eval_m1` (keyword only) keeps
//! them out of the 13/14 denominator and only records the miss. The real
//! bge-m3 run is `eval_m1_omlx` (`#[ignore]`, not CI):
//!
//! ```text
//! export PATH="$HOME/Library/Application Support/mbx/bin:$PATH"
//! export OMLX_URL=http://127.0.0.1:8088
//! export OMLX_API_KEY="$(python3 -c 'import json,os; print(json.load(open(os.path.expanduser("~/.omlx/settings.json")))["auth"]["api_key"])')"
//! cd rust && cargo test -j 4 -p agent24-memory eval_m1::eval_m1_omlx -- --ignored --exact --nocapture
//! ```

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
/// Hybrid eval must exercise the vector route on at least this many queries.
const MIN_VECTOR_ROUTE_QUERIES: usize = 3;
const PARAPHRASE: &str = "paraphrase";
const PARAPHRASE_NO_ANSWER: &str = "paraphrase_no_answer";
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
    // VR-5: one fact per paraphrase case. The question uses different words
    // (no shared content bigram); the test embedder links them by CONCEPTS.
    f("p01-piano", OWNER, "每周三晚上练钢琴"),
    f("p02-alarm", OWNER, "闹钟设在清晨六点"),
    f("p03-neuter", OWNER, "宠物猫已做绝育"),
    f("p04-swim", OWNER, "每周五下班去游泳"),
    f("p05-neck", OWNER, "颈椎病好多年"),
    f("p06-loan", OWNER, "每月还房贷八千"),
    f("p07-hike", OWNER, "周末固定去爬山"),
    f("p08-uncle", OWNER, "紧急联系人是舅舅"),
    f("p09-glasses", OWNER, "眼镜度数是三百"),
    f("p10-license", OWNER, "驾照档案号尾段是庚寅"),
    f("p11-desk", OWNER, "工位在靠窗第三排"),
    f("p12-soy", OWNER, "早餐只喝豆浆"),
    f("p13-south", OWNER, "租的房子朝南"),
    f("p14-school", OWNER, "孩子就读于育才小学"),
    f("p15-mail", OWNER, "常用邮箱前缀是青石"),
    f("p16-checkup", OWNER, "每年体检安排在三月"),
    f("p17-screen", OWNER, "屏幕贴的是磨砂膜"),
    f("p18-metro", OWNER, "地铁通勤四十分钟"),
    f("p19-pen", OWNER, "惯用左手执笔"),
    f("p20-rent", OWNER, "每年四月交房租"),
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
        // VR-5 paraphrases. Each query is a different wording of the fixture
        // above and shares no content bigram with that fixture.
        EvalCase { id: "p01-piano", category: PARAPHRASE, query: "练琴固定哪一天", expected_ids: &["p01-piano"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p02-alarm", category: PARAPHRASE, query: "每天几点被叫醒", expected_ids: &["p02-alarm"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p03-neuter", category: PARAPHRASE, query: "猫咪做了节育没有", expected_ids: &["p03-neuter"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p04-swim", category: PARAPHRASE, query: "周末前有固定的泳程吗", expected_ids: &["p04-swim"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p05-neck", category: PARAPHRASE, query: "脖子的老毛病几年了", expected_ids: &["p05-neck"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p06-loan", category: PARAPHRASE, query: "月供大概是多少钱", expected_ids: &["p06-loan"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p07-hike", category: PARAPHRASE, query: "假日专门去登山吗", expected_ids: &["p07-hike"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p08-uncle", category: PARAPHRASE, query: "出事最先找哪位亲戚", expected_ids: &["p08-uncle"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p09-glasses", category: PARAPHRASE, query: "近视多少度", expected_ids: &["p09-glasses"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p10-license", category: PARAPHRASE, query: "驾驶证编号末尾是什么", expected_ids: &["p10-license"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p11-desk", category: PARAPHRASE, query: "座位是不是挨着窗", expected_ids: &["p11-desk"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p12-soy", category: PARAPHRASE, query: "早起那一顿喝什么", expected_ids: &["p12-soy"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p13-south", category: PARAPHRASE, query: "住房朝向是哪边", expected_ids: &["p13-south"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p14-school", category: PARAPHRASE, query: "小朋友在哪所学校上学", expected_ids: &["p14-school"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p15-mail", category: PARAPHRASE, query: "邮件地址怎么开头", expected_ids: &["p15-mail"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p16-checkup", category: PARAPHRASE, query: "健康检查固定哪个月", expected_ids: &["p16-checkup"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p17-screen", category: PARAPHRASE, query: "显示器蒙了哪种膜", expected_ids: &["p17-screen"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p18-metro", category: PARAPHRASE, query: "坐轨道上下班要多久", expected_ids: &["p18-metro"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p19-pen", category: PARAPHRASE, query: "写字用哪一侧的手", expected_ids: &["p19-pen"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "p20-rent", category: PARAPHRASE, query: "租约费用哪个月付", expected_ids: &["p20-rent"], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        // No lexical overlap with the corpus, and no CONCEPTS word, so neither
        // FTS nor the test embedder may inject a fact.
        EvalCase { id: "pn1-quantum", category: PARAPHRASE_NO_ANSWER, query: "量子纠缠怎么推导", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "pn2-stew", category: PARAPHRASE_NO_ANSWER, query: "红烧肉要炖到软烂", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "pn3-wallet", category: PARAPHRASE_NO_ANSWER, query: "比特币钱包助记词放哪", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
        EvalCase { id: "pn4-poem", category: PARAPHRASE_NO_ANSWER, query: "这首古诗的下一句", expected_ids: &[], security_forbidden_ids: &[], distractor_forbidden_ids: &[] },
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
    paraphrase_total: usize,
    paraphrase_hits: usize,
    /// Same paraphrase queries scored with keyword search, when the run is
    /// also scoring the hybrid path (the real-model comparison).
    paraphrase_fts_hits: usize,
    paraphrase_no_answer: usize,
    paraphrase_no_answer_injected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Keyword search only. Paraphrases stay out of the Hit@5 denominator.
    Fts,
    /// CI hybrid: [`NgramEmbedder`]. Paraphrases join the 13/14 denominator.
    Hybrid,
    /// Real bge-m3. Same gates as [`Mode::Hybrid`], plus the FTS comparison.
    Real,
}

fn pct(n: usize, of: usize) -> String {
    if of == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / of as f64)
    }
}

async fn run_eval(mode: Mode) {
    let cases = cases();
    let paraphrase_cases = cases
        .iter()
        .filter(|case| case.category == PARAPHRASE)
        .count();
    let paraphrase_negatives = cases
        .iter()
        .filter(|case| case.category == PARAPHRASE_NO_ANSWER)
        .count();
    assert!(
        cases.len() >= 15,
        "M1-T08 asks for >=15 hand-written cases: got {}",
        cases.len()
    );
    assert!(
        paraphrase_cases >= 20,
        "VR-5 asks for >=20 Chinese paraphrase cases: got {paraphrase_cases}"
    );
    assert!(
        paraphrase_negatives >= 4,
        "VR-5 asks for a few no-answer prompts: got {paraphrase_negatives}"
    );
    assert!(
        DISTRACTOR_IDS.len() >= 5,
        "M1-T08 asks for >=5 distractor assertions"
    );

    let store = KvStore::open_memory().await.unwrap();
    seed(&store).await;
    let retriever = store.retriever();
    // Hybrid mode: every current fact is embedded, and the SAME cases run
    // through `HybridRetriever` (FTS 20 + vector 20 + RRF, MIN_COSINE).
    let hybrid_retriever = match mode {
        Mode::Fts => None,
        Mode::Hybrid => {
            store
                .vector_retriever(NgramEmbedder)
                .reindex(OWNER)
                .await
                .unwrap();
            Some(store.hybrid_retriever(std::sync::Arc::new(NgramEmbedder)))
        }
        Mode::Real => {
            let embedder = crate::embedder::LocalEmbedder::open(&store, omlx_client())
                .await
                .expect("oMLX embedder did not open; is bge-m3 listening?");
            store
                .vector_retriever(embedder.clone())
                .reindex(OWNER)
                .await
                .unwrap();
            // A cold embed is slower than the production 1.5s budget. The
            // ignored run only records quality, so give the request room and
            // keep the breaker from dropping the rest of the set.
            Some(
                store
                    .hybrid_retriever(std::sync::Arc::new(embedder))
                    .with_timing(
                        std::time::Duration::from_secs(60),
                        std::time::Duration::from_secs(30),
                    ),
            )
        }
    };
    let hybrid = hybrid_retriever.is_some();

    let mut metrics = Metrics::default();
    println!(
        "—— M1-T08 + T07.2 recall eval baseline ({}, top_{TOP_K}) ——",
        match mode {
            Mode::Fts => "search_any",
            Mode::Hybrid => "hybrid",
            Mode::Real => "hybrid-omlx",
        }
    );
    let mut vector_route_queries = 0usize;
    for case in &cases {
        if let Some(h) = &hybrid_retriever {
            // Did the vector route itself produce candidates (cosine >= MIN_COSINE)?
            if !h.vector_route(case.query, OWNER).await.unwrap().is_empty() {
                vector_route_queries += 1;
            }
        }
        let fts_ids: Vec<String> = if mode == Mode::Real {
            retriever
                .search_any(case.query, OWNER, TOP_K)
                .await
                .unwrap()
                .into_iter()
                .map(|hit| hit.assertion.id)
                .collect()
        } else {
            Vec::new()
        };
        let hits = match &hybrid_retriever {
            Some(h) => h.search(case.query, OWNER, TOP_K).await.unwrap(),
            None => retriever
                .search_any(case.query, OWNER, TOP_K)
                .await
                .unwrap(),
        };
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

        if case.category == PARAPHRASE_NO_ANSWER {
            // Before the empty-expected bucket: these are a hard gate, not
            // the record-only Chinese no-answer rate.
            metrics.paraphrase_no_answer += 1;
            metrics.paraphrase_no_answer_injected += usize::from(!hit_ids.is_empty());
        } else if case.expected_ids.is_empty() {
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
        } else if case.category == PARAPHRASE {
            // Keyword search misses these on purpose. Only the hybrid runs
            // fold them into the 13/14 Hit@5 denominator.
            metrics.paraphrase_total += 1;
            metrics.paraphrase_hits += usize::from(hit_at_5);
            if mode == Mode::Real {
                let fts_hit = fts_ids
                    .iter()
                    .any(|id| case.expected_ids.contains(&id.as_str()));
                metrics.paraphrase_fts_hits += usize::from(fts_hit);
            }
            if hybrid {
                metrics.answerable += 1;
                metrics.hit_at_1 += usize::from(hit_at_1);
                metrics.hit_at_5 += usize::from(hit_at_5);
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
    if hybrid {
        println!(
            "queries whose vector route returned candidates: {vector_route_queries}/{}",
            cases.len()
        );
        assert!(
            vector_route_queries >= MIN_VECTOR_ROUTE_QUERIES,
            "hybrid eval silently fell back to FTS: only {vector_route_queries} queries crossed MIN_COSINE"
        );
    }
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
    println!(
        "paraphrase Hit@5 ({}): {}/{} ({})",
        if hybrid {
            "in the 13/14 denominator"
        } else {
            "FTS record only; not in the 13/14 denominator"
        },
        metrics.paraphrase_hits,
        metrics.paraphrase_total,
        pct(metrics.paraphrase_hits, metrics.paraphrase_total)
    );
    if mode == Mode::Real {
        println!(
            "paraphrase Hit@5 pure FTS (comparison): {}/{} ({})",
            metrics.paraphrase_fts_hits,
            metrics.paraphrase_total,
            pct(metrics.paraphrase_fts_hits, metrics.paraphrase_total)
        );
    }
    println!(
        "paraphrase no-answer injection (HARD GATE = 0): {}/{} ({})",
        metrics.paraphrase_no_answer_injected,
        metrics.paraphrase_no_answer,
        pct(
            metrics.paraphrase_no_answer_injected,
            metrics.paraphrase_no_answer
        )
    );

    if mode == Mode::Real {
        print_real_margins(&store).await;
    }

    assert_eq!(
        metrics.security_leaks, 0,
        "security leak (cross-owner / superseded / retracted / unqualified) must be zero"
    );
    assert_eq!(
        metrics.no_answer_en_with_any_hit, 0,
        "an English/neutral no-answer prompt must never inject a memory (M1-T07.2)"
    );
    assert!(
        metrics.paraphrase_no_answer >= 4,
        "VR-5 no-answer prompts were not scored"
    );
    assert_eq!(
        metrics.paraphrase_no_answer_injected, 0,
        "a VR-5 no-answer prompt must not inject a memory"
    );
    assert!(
        metrics.hit_at_5 * 14 >= 13 * metrics.answerable,
        "Hit@5 must be >= 13/14 of the {} answerable cases: got {}/{}",
        metrics.answerable,
        metrics.hit_at_5,
        metrics.answerable
    );
    if hybrid {
        assert!(
            metrics.paraphrase_hits * 14 >= 13 * metrics.paraphrase_total,
            "paraphrase Hit@5 must be >= 13/14: got {}/{}",
            metrics.paraphrase_hits,
            metrics.paraphrase_total
        );
    }
    assert_eq!(
        metrics.cjk_safety_hits, metrics.cjk_safety_total,
        "every CJK safety-bigram case (a real word that merely contains a \
         pronoun/particle character) must be recalled (M1-T07.2 round 2 M2)"
    );
    if mode == Mode::Real {
        assert!(
            metrics.paraphrase_hits > metrics.paraphrase_fts_hits,
            "hybrid paraphrase Hit@5 ({}) must beat pure FTS ({})",
            metrics.paraphrase_hits,
            metrics.paraphrase_fts_hits
        );
    }
}

/// A stand-in for a real model with a tiny synonym table: hashed character
/// bigrams of the OBJECT text (the fixed `user said_to_remember` prefix that
/// reindex prepends is stripped, or it would dominate every vector) plus a
/// concept axis per keyword, so paraphrases share a concept and cross
/// `MIN_COSINE` while unrelated text stays far below it.
#[derive(Clone)]
struct NgramEmbedder;

const CONCEPTS: &[&[&str]] = &[
    &["过敏"],
    &["狗"],
    &["工作", "公司"],
    &["颜色"],
    // VR-5: fact wording, then the question's wording. They share no content
    // bigram, so the axis is the only reason the pair clears MIN_COSINE.
    &["钢琴", "练琴"],
    &["闹钟", "叫醒"],
    &["绝育", "节育"],
    &["游泳", "泳程"],
    &["颈椎", "脖子"],
    &["房贷", "月供"],
    &["爬山", "登山"],
    &["舅舅", "亲戚"],
    &["眼镜", "近视"],
    &["驾照", "驾驶证"],
    &["工位", "挨着"],
    &["豆浆", "早起"],
    &["朝南", "朝向"],
    &["育才", "小朋友"],
    &["邮箱", "邮件"],
    &["体检", "健康"],
    &["磨砂", "显示器"],
    &["地铁", "轨道"],
    &["执笔", "写字"],
    &["房租", "租约"],
];
/// Heavy enough that one shared axis clears 0.62 with no shared bigram.
/// A query that matches no axis stays far below that line.
const CONCEPT_WEIGHT: f32 = 6.0;
const NGRAM_DIMS: usize = 512;
#[cfg(test)]
const FACT_PREFIX: &str = "user said_to_remember ";

#[async_trait::async_trait]
impl crate::vector::Embedder for NgramEmbedder {
    fn model_id(&self) -> &str {
        "ngram"
    }
    fn revision(&self) -> &str {
        "v1"
    }
    async fn embed(&self, text: &str) -> crate::Result<crate::vector::Embedding> {
        let text = text.strip_prefix(FACT_PREFIX).unwrap_or(text);
        let mut vector = vec![0.0f32; NGRAM_DIMS + CONCEPTS.len()];
        let chars: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
        for pair in chars.windows(2) {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for c in pair {
                h ^= u64::from(u32::from(*c));
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            vector[(h as usize) % NGRAM_DIMS] += 1.0;
        }
        for (i, words) in CONCEPTS.iter().enumerate() {
            if words.iter().any(|w| text.contains(w)) {
                vector[NGRAM_DIMS + i] = CONCEPT_WEIGHT;
            }
        }
        Ok(crate::vector::Embedding {
            model_id: "ngram".into(),
            revision: "v1".into(),
            dims: vector.len() as u32,
            normalized: false,
            vector,
        })
    }
}

/// Same URL and key read as chat (`OMLX_URL` / `OMLX_API_KEY`, loopback only).
/// Ignores `IDORIS_URL` so this run always hits the local oMLX named above.
fn omlx_client() -> agent24_models::EmbeddingClient {
    let url = std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
    let key = std::env::var("OMLX_API_KEY").unwrap_or_else(|_| "xiaobao8088".to_owned());
    agent24_models::EmbeddingClient::direct(url, key, agent24_models::DEFAULT_EMBED_MODEL)
        .expect("OMLX_URL must be a loopback embeddings endpoint")
}

/// Min cosine of each paraphrase against its own fact, and max cosine of each
/// no-answer prompt against the owner's vectors. Printed so VR-5 can see
/// whether [`crate::hybrid::MIN_COSINE`] still sits between the two.
async fn print_real_margins(store: &KvStore) {
    use crate::vector::Embedder;
    let embedder = omlx_embedder(store).await;
    let rows = stored_vectors(store, &embedder).await;
    let mut paraphrase_min = f32::MAX;
    let mut paraphrase_min_id = "";
    let mut no_answer_max = f32::MIN;
    let mut no_answer_max_query = "";
    for case in cases() {
        let query_vec = embedder.embed(case.query).await.unwrap().vector;
        if case.category == PARAPHRASE {
            let expected = case.expected_ids[0];
            let stored = rows
                .iter()
                .find(|(id, _)| id == expected)
                .map(|(_, vector)| vector.as_slice())
                .unwrap_or(&[]);
            let score = crate::vector::cosine(&query_vec, stored);
            if score < paraphrase_min {
                paraphrase_min = score;
                paraphrase_min_id = case.id;
            }
            println!(
                "calibration paraphrase {id} cosine={score:.3}",
                id = case.id
            );
        } else if case.category == PARAPHRASE_NO_ANSWER {
            let score = rows
                .iter()
                .map(|(_, vector)| crate::vector::cosine(&query_vec, vector))
                .fold(f32::MIN, f32::max);
            if score > no_answer_max {
                no_answer_max = score;
                no_answer_max_query = case.query;
            }
            println!(
                "calibration no-answer {id} max_cosine={score:.3}",
                id = case.id
            );
        }
    }
    println!(
        "calibration summary: paraphrase min={paraphrase_min:.3} ({paraphrase_min_id}), no-answer max={no_answer_max:.3} ({no_answer_max_query}), MIN_COSINE={}",
        crate::hybrid::MIN_COSINE
    );
}

async fn omlx_embedder(store: &KvStore) -> crate::embedder::LocalEmbedder {
    crate::embedder::LocalEmbedder::open(store, omlx_client())
        .await
        .unwrap()
}

async fn stored_vectors(
    store: &KvStore,
    embedder: &crate::embedder::LocalEmbedder,
) -> Vec<(String, Vec<f32>)> {
    use crate::vector::Embedder;
    use sqlx::Row;
    let revision = crate::vector::index_revision(embedder.revision());
    let rows = sqlx::query(
        "SELECT e.assertion_id, e.vec FROM mem_embeddings e
         JOIN mem_assertions a ON a.id = e.assertion_id
         WHERE a.scope_owner = ? AND e.model_id = ? AND e.revision = ?
           AND a.recorded_to IS NULL AND a.qualified = 1",
    )
    .bind(OWNER)
    .bind(embedder.model_id())
    .bind(&revision)
    .fetch_all(crate::test_hooks::pool(store))
    .await
    .unwrap();
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("assertion_id");
            let vector = crate::vector::blob_to_vec(&row.get::<Vec<u8>, _>("vec"));
            (id, vector)
        })
        .collect()
}

#[tokio::test]
async fn eval_m1() {
    run_eval(Mode::Fts).await;
}

/// VR-3 / VR-5: both hard gates (Hit@5 >= 13/14 of the set that now includes
/// the paraphrases, no-answer injects nothing) hold on the test embedder.
#[tokio::test]
async fn eval_m1_hybrid() {
    run_eval(Mode::Hybrid).await;
}

/// Real bge-m3 on the local oMLX. Not part of CI. Command in the module docs.
#[tokio::test]
#[ignore = "real oMLX bge-m3-mlx-fp16; set OMLX_URL and OMLX_API_KEY, then --ignored --exact"]
async fn eval_m1_omlx() {
    run_eval(Mode::Real).await;
}
