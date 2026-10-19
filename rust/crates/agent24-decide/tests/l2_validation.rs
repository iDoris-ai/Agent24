//! D1-2b L2 answer validation: fixtures and acceptance matrix from
//! `docs/agent/PLAN-DECIDE-D1-2B-L2-VALIDATION.md`, exercised only through the
//! crate's public API (so `InvalidRequest` must be re-exported at the root).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent24_decide::{
    Answer, AnswerValue, BackendKind, BackendOutcome, Decision, DecisionBackend, DecisionRequest,
    DecisionService, InvalidRequest, Outcome, Question,
};
use async_trait::async_trait;

const Q_JSON: &str = r#"{"point":"retain.intent","input":"","context":{},"questions":[{"kind":"noul","id":"n","prompt":""},{"kind":"choice","id":"c","prompt":"","labels":["yes","no"]},{"kind":"score","id":"s","prompt":"","min":-1.0,"max":1.0}],"side_effect":"reversible"}"#;

const ALL_KINDS: [BackendKind; 4] = [
    BackendKind::Rule,
    BackendKind::Encoder,
    BackendKind::Deep,
    BackendKind::LlmSimulation,
];

fn q() -> DecisionRequest {
    serde_json::from_str(Q_JSON).expect("fixture Q parses")
}

fn ans(id: &str, value: AnswerValue, p: Option<f32>, kind: BackendKind, cal: bool) -> Answer {
    Answer::new(id, value, p, kind, cal)
}

/// N=("n",Bool(true),None,false)
fn n(kind: BackendKind) -> Answer {
    ans("n", AnswerValue::Bool(true), None, kind, false)
}

/// C=("c",Label("yes"),Some(1.0),true)
fn c(kind: BackendKind) -> Answer {
    ans(
        "c",
        AnswerValue::Label("yes".to_owned()),
        Some(1.0),
        kind,
        true,
    )
}

/// S=("s",Score(0.0),Some(0.0),false)
fn s(kind: BackendKind) -> Answer {
    ans("s", AnswerValue::Score(0.0), Some(0.0), kind, false)
}

fn x(kind: BackendKind) -> Answer {
    ans("x", AnswerValue::Bool(true), None, kind, false)
}

fn full(kind: BackendKind) -> Vec<Answer> {
    vec![n(kind), c(kind), s(kind)]
}

#[derive(Clone, Default)]
struct Calls {
    kind: Arc<AtomicUsize>,
    evaluate: Arc<AtomicUsize>,
}

impl Calls {
    fn kind(&self) -> usize {
        self.kind.load(Ordering::SeqCst)
    }
    fn evaluate(&self) -> usize {
        self.evaluate.load(Ordering::SeqCst)
    }
}

/// A counting fake backend that can report any kind (a Rule floor here is not
/// the real `RuleBackend`'s miss).
struct Fake {
    kind: BackendKind,
    calls: Calls,
    outcome: BackendOutcome,
}

#[async_trait]
impl DecisionBackend for Fake {
    fn kind(&self) -> BackendKind {
        self.calls.kind.fetch_add(1, Ordering::SeqCst);
        self.kind
    }

    async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
        self.calls.evaluate.fetch_add(1, Ordering::SeqCst);
        self.outcome.clone()
    }
}

fn chain(stages: Vec<(BackendKind, BackendOutcome)>) -> (DecisionService, Vec<Calls>) {
    let mut calls = Vec::new();
    let mut backends: Vec<Arc<dyn DecisionBackend>> = Vec::new();
    for (kind, outcome) in stages {
        let c = Calls::default();
        calls.push(c.clone());
        backends.push(Arc::new(Fake {
            kind,
            calls: c,
            outcome,
        }));
    }
    (DecisionService::new(backends), calls)
}

async fn run(request: &DecisionRequest, stages: Vec<(BackendKind, BackendOutcome)>) -> Decision {
    let (service, _) = chain(stages);
    service.decide(request).await.expect("request is valid")
}

fn invalid_response(code: &str, index: usize, kind: BackendKind) -> Outcome {
    Outcome::InvalidResponse {
        code: code.to_owned(),
        failed_stage_index: index,
        failed_backend_kind: kind,
    }
}

// ---------------------------------------------------------------- requests

async fn assert_request_rejected(request: DecisionRequest, code: &str) {
    let (service, calls) = chain(vec![
        (
            BackendKind::Rule,
            BackendOutcome::Decided(full(BackendKind::Rule)),
        ),
        (
            BackendKind::Encoder,
            BackendOutcome::Decided(full(BackendKind::Encoder)),
        ),
    ]);
    let err = service
        .decide(&request)
        .await
        .expect_err("request must be rejected");
    assert_eq!(
        err,
        InvalidRequest {
            code: code.to_owned()
        }
    );
    for (i, c) in calls.iter().enumerate() {
        assert_eq!(c.kind(), 0, "stage {i} kind() must not be called ({code})");
        assert_eq!(
            c.evaluate(),
            0,
            "stage {i} evaluate() must not be called ({code})"
        );
    }
}

fn with_question(i: usize, question: Question) -> DecisionRequest {
    let mut r = q();
    r.questions[i] = question;
    r
}

fn choice_with(labels: &[&str]) -> DecisionRequest {
    with_question(
        1,
        Question::Choice {
            id: "c".to_owned(),
            prompt: String::new(),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        },
    )
}

fn score_with(min: f32, max: f32) -> DecisionRequest {
    with_question(
        2,
        Question::Score {
            id: "s".to_owned(),
            prompt: String::new(),
            min,
            max,
        },
    )
}

fn noul_with_id(id: &str) -> DecisionRequest {
    with_question(
        0,
        Question::Noul {
            id: id.to_owned(),
            prompt: String::new(),
        },
    )
}

#[tokio::test]
async fn request_empty_questions_is_rejected_even_without_backends() {
    let mut r = q();
    r.questions.clear();
    assert_request_rejected(r.clone(), "empty_questions").await;
    let err = DecisionService::new(vec![])
        .decide(&r)
        .await
        .expect_err("no backends still validates first");
    assert_eq!(err.code, "empty_questions");
}

#[tokio::test]
async fn request_blank_and_duplicate_question_ids_are_rejected() {
    assert_request_rejected(noul_with_id(""), "blank_question_id").await;
    assert_request_rejected(noul_with_id(" \t"), "blank_question_id").await;
    let mut dup = q();
    dup.questions[1] = Question::Noul {
        id: "n".to_owned(),
        prompt: String::new(),
    };
    assert_request_rejected(dup, "duplicate_question_id").await;
}

#[tokio::test]
async fn request_bad_labels_are_rejected() {
    assert_request_rejected(choice_with(&[]), "empty_labels").await;
    assert_request_rejected(choice_with(&["yes", ""]), "blank_label").await;
    assert_request_rejected(choice_with(&["yes", " "]), "blank_label").await;
    assert_request_rejected(choice_with(&["yes", "yes"]), "duplicate_label").await;
}

#[tokio::test]
async fn request_bad_score_bounds_are_rejected() {
    for (min, max) in [
        (1.0, -1.0),
        (f32::NAN, 1.0),
        (-1.0, f32::INFINITY),
        (f32::NEG_INFINITY, 1.0),
    ] {
        assert_request_rejected(score_with(min, max), "invalid_score_bounds").await;
    }
}

#[tokio::test]
async fn request_check_order_reports_first_error_only() {
    // Question order wins: a blank ID on q0 beats bad labels on q1.
    let mut r = choice_with(&[]);
    r.questions[0] = Question::Noul {
        id: String::new(),
        prompt: String::new(),
    };
    assert_request_rejected(r, "blank_question_id").await;
    // Within one question: ID before labels; labels checked in order.
    let r = with_question(
        1,
        Question::Choice {
            id: "n".to_owned(),
            prompt: String::new(),
            labels: vec![],
        },
    );
    assert_request_rejected(r, "duplicate_question_id").await;
    assert_request_rejected(choice_with(&["", "yes", "yes"]), "blank_label").await;
    assert_request_rejected(choice_with(&["yes", "yes", ""]), "duplicate_label").await;
}

#[tokio::test]
async fn request_open_fields_and_edge_shapes_pass() {
    let r: DecisionRequest = serde_json::from_str(
        r#"{"point":"no.such.point","input":"","context":null,"questions":[{"kind":"noul","id":"n","prompt":""},{"kind":"choice","id":"c","prompt":"","labels":["yes"]},{"kind":"score","id":"s","prompt":"","min":0.0,"max":0.0}],"side_effect":"irreversible"}"#,
    )
    .unwrap();
    let answers = full(BackendKind::Encoder);
    let d = run(
        &r,
        vec![(
            BackendKind::Encoder,
            BackendOutcome::Decided(answers.clone()),
        )],
    )
    .await;
    assert_eq!(d.outcome, Outcome::Decided);
    assert_eq!(d.answers, answers);
}

#[tokio::test]
async fn ids_and_labels_are_not_trimmed() {
    let r: DecisionRequest = serde_json::from_str(
        r#"{"point":"retain.intent","input":"","context":{},"questions":[{"kind":"noul","id":"n","prompt":""},{"kind":"noul","id":" n","prompt":""},{"kind":"choice","id":"c","prompt":"","labels":["yes"," yes"]}],"side_effect":"reversible"}"#,
    )
    .unwrap();
    let k = BackendKind::Encoder;
    let good = vec![
        n(k),
        ans(" n", AnswerValue::Bool(false), None, k, false),
        ans("c", AnswerValue::Label(" yes".to_owned()), None, k, false),
    ];
    let d = run(&r, vec![(k, BackendOutcome::Decided(good.clone()))]).await;
    assert_eq!(d.outcome, Outcome::Decided);
    assert_eq!(d.answers, good);

    let mut bad_id = good.clone();
    bad_id[1] = ans("n ", AnswerValue::Bool(false), None, k, false);
    let d = run(&r, vec![(k, BackendOutcome::Decided(bad_id))]).await;
    assert_eq!(d.outcome, invalid_response("unknown_question_id", 0, k));

    let mut bad_label = good;
    bad_label[2] = ans("c", AnswerValue::Label("yes ".to_owned()), None, k, false);
    let d = run(&r, vec![(k, BackendOutcome::Decided(bad_label))]).await;
    assert_eq!(d.outcome, invalid_response("label_not_allowed", 0, k));
}

// ---------------------------------------------------------------- responses

#[derive(Clone, Copy, Debug)]
enum Mode {
    Decided,
    Floor,
}

fn wrap(mode: Mode, answers: Vec<Answer>) -> BackendOutcome {
    match mode {
        Mode::Decided => BackendOutcome::Decided(answers),
        Mode::Floor => BackendOutcome::NoConclusion { floor: answers },
    }
}

/// The response negative path for one mode: the bad layer runs exactly once,
/// the following (successful) layer is never called, the malformed set is
/// discarded whole and there is no earlier floor.
async fn assert_response_rejected_in(
    mode: Mode,
    kind: BackendKind,
    answers: Vec<Answer>,
    code: &str,
) {
    let (service, calls) = chain(vec![
        (kind, wrap(mode, answers)),
        (
            BackendKind::Deep,
            BackendOutcome::Decided(full(BackendKind::Deep)),
        ),
    ]);
    let d = service.decide(&q()).await.expect("request is valid");
    assert_eq!(d.outcome, invalid_response(code, 0, kind), "{mode:?}");
    assert!(
        d.answers.is_empty(),
        "{mode:?}: malformed set is not salvaged"
    );
    assert_eq!(d.backend, kind, "{mode:?}: no floor => failed layer");
    assert_eq!(d.model, None);
    assert_eq!(calls[0].evaluate(), 1, "{mode:?}");
    assert_eq!(calls[1].evaluate(), 0, "{mode:?}: next layer must not run");
}

async fn assert_response_rejected(kind: BackendKind, answers: Vec<Answer>, code: &str) {
    assert_response_rejected_in(Mode::Decided, kind, answers.clone(), code).await;
    assert_response_rejected_in(Mode::Floor, kind, answers, code).await;
}

async fn assert_response_accepted(kind: BackendKind, answers: Vec<Answer>) {
    // Decided mode needs full coverage; floor mode accepts any valid subset.
    let d = run(&q(), vec![(kind, BackendOutcome::Decided(answers.clone()))]).await;
    assert_eq!(d.outcome, Outcome::Decided);
    assert_eq!(d.backend, kind);
    assert_eq!(d.answers.len(), answers.len());
    let d = run(
        &q(),
        vec![(
            kind,
            BackendOutcome::NoConclusion {
                floor: answers.clone(),
            },
        )],
    )
    .await;
    assert!(matches!(d.outcome, Outcome::Abstain { .. }));
    assert_eq!(d.backend, kind);
    assert_eq!(d.answers.len(), answers.len());
}

fn replace(mut answers: Vec<Answer>, i: usize, a: Answer) -> Vec<Answer> {
    answers[i] = a;
    answers
}

#[tokio::test]
async fn decided_in_any_order_is_kept_verbatim_even_after_a_floor() {
    let e = BackendKind::Encoder;
    let scn = vec![s(e), c(e), n(e)];
    let d = run(
        &q(),
        vec![
            (
                BackendKind::Rule,
                BackendOutcome::NoConclusion {
                    floor: vec![n(BackendKind::Rule)],
                },
            ),
            (e, BackendOutcome::Decided(scn.clone())),
        ],
    )
    .await;
    assert_eq!(d.outcome, Outcome::Decided);
    assert_eq!(d.backend, e);
    assert_eq!(d.answers, scn);
}

#[tokio::test]
async fn decided_must_cover_every_question() {
    let e = BackendKind::Encoder;
    for answers in [vec![], vec![n(e), c(e)]] {
        assert_response_rejected_in(Mode::Decided, e, answers, "answer_coverage").await;
    }
}

#[tokio::test]
async fn unknown_and_duplicate_answer_ids_are_rejected() {
    let e = BackendKind::Encoder;
    let mut with_x = full(e);
    with_x.push(x(e));
    assert_response_rejected(e, with_x, "unknown_question_id").await;
    assert_response_rejected(e, vec![n(e), n(e)], "duplicate_answer_id").await;
}

#[tokio::test]
async fn answer_kind_must_match_question_kind() {
    let e = BackendKind::Encoder;
    let label = AnswerValue::Label("yes".to_owned());
    assert_response_rejected(
        e,
        replace(full(e), 0, ans("n", label, None, e, false)),
        "answer_kind",
    )
    .await;
    assert_response_rejected(
        e,
        replace(
            full(e),
            1,
            ans("c", AnswerValue::Bool(true), None, e, false),
        ),
        "answer_kind",
    )
    .await;
    assert_response_rejected(
        e,
        replace(
            full(e),
            2,
            ans("s", AnswerValue::Bool(true), None, e, false),
        ),
        "answer_kind",
    )
    .await;
}

#[tokio::test]
async fn labels_must_match_exactly() {
    let e = BackendKind::Encoder;
    for bad in ["YES", " yes", "maybe"] {
        let a = ans("c", AnswerValue::Label(bad.to_owned()), Some(1.0), e, true);
        assert_response_rejected(e, replace(full(e), 1, a), "label_not_allowed").await;
    }
}

#[tokio::test]
async fn scores_must_be_finite_and_within_closed_bounds() {
    let e = BackendKind::Encoder;
    for ok in [-1.0_f32, 1.0] {
        let a = ans("s", AnswerValue::Score(ok), Some(0.0), e, false);
        assert_response_accepted(e, replace(full(e), 2, a)).await;
    }
    for bad in [-1.01_f32, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let a = ans("s", AnswerValue::Score(bad), Some(0.0), e, false);
        assert_response_rejected(e, replace(full(e), 2, a), "score_out_of_range").await;
    }
}

#[tokio::test]
async fn valid_probabilities_pass_and_are_kept_as_is() {
    for kind in ALL_KINDS {
        for p in [None, Some(0.0_f32), Some(1.0)] {
            for cal in [true, false] {
                let a = ans("n", AnswerValue::Bool(true), p, kind, cal);
                let answers = replace(full(kind), 0, a);
                let d = run(&q(), vec![(kind, BackendOutcome::Decided(answers))]).await;
                assert_eq!(d.outcome, Outcome::Decided);
                assert_eq!(d.answers[0].p(), p, "None is not invented as 0/1");
                let expect_cal = cal && kind != BackendKind::LlmSimulation;
                assert_eq!(d.answers[0].calibrated(), expect_cal);
                let d = run(
                    &q(),
                    vec![(
                        kind,
                        BackendOutcome::NoConclusion {
                            floor: vec![ans("n", AnswerValue::Bool(true), p, kind, cal)],
                        },
                    )],
                )
                .await;
                assert_eq!(d.answers[0].p(), p);
            }
        }
    }
}

#[tokio::test]
async fn invalid_probabilities_are_rejected_for_every_backend_kind() {
    for kind in ALL_KINDS {
        for p in [-0.01_f32, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for cal in [true, false] {
                let a = ans("n", AnswerValue::Bool(true), Some(p), kind, cal);
                assert_response_rejected(kind, replace(full(kind), 0, a), "invalid_probability")
                    .await;
            }
        }
    }
}

/// A backend whose real `kind()` is LlmSimulation but which builds its
/// answers claiming Encoder + calibrated=true.
struct LyingLlm(Vec<Answer>);

#[async_trait]
impl DecisionBackend for LyingLlm {
    fn kind(&self) -> BackendKind {
        BackendKind::LlmSimulation
    }
    async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
        BackendOutcome::Decided(self.0.clone())
    }
}

#[tokio::test]
async fn llm_simulation_is_restamped_uncalibrated_and_still_range_checked() {
    let e = BackendKind::Encoder;
    let lying = vec![
        ans("n", AnswerValue::Bool(true), Some(0.9), e, true),
        ans("c", AnswerValue::Label("no".to_owned()), Some(0.8), e, true),
        ans("s", AnswerValue::Score(0.5), Some(0.7), e, true),
    ];
    let d = DecisionService::new(vec![Arc::new(LyingLlm(lying))])
        .decide(&q())
        .await
        .unwrap();
    assert_eq!(d.outcome, Outcome::Decided);
    assert_eq!(d.backend, BackendKind::LlmSimulation);
    assert!(d.answers.iter().all(|a| !a.calibrated()));

    let bad = vec![
        ans("n", AnswerValue::Bool(true), Some(1.5), e, true),
        ans("c", AnswerValue::Label("no".to_owned()), Some(0.8), e, true),
        ans("s", AnswerValue::Score(0.5), Some(0.7), e, true),
    ];
    let d = DecisionService::new(vec![Arc::new(LyingLlm(bad))])
        .decide(&q())
        .await
        .unwrap();
    assert_eq!(
        d.outcome,
        invalid_response("invalid_probability", 0, BackendKind::LlmSimulation)
    );
}

#[tokio::test]
async fn any_valid_floor_subset_is_kept_and_never_promoted() {
    let e = BackendKind::Encoder;
    for floor in [vec![], vec![n(e)], vec![s(e), n(e)], vec![s(e), c(e), n(e)]] {
        let d = run(
            &q(),
            vec![(
                e,
                BackendOutcome::NoConclusion {
                    floor: floor.clone(),
                },
            )],
        )
        .await;
        assert_eq!(
            d.outcome,
            Outcome::Abstain {
                reason: "no backend reached a conclusion".to_owned()
            },
            "exhausted chain abstains, even with a full floor"
        );
        assert_eq!(d.answers, floor, "order kept");
        let expected_backend = if floor.is_empty() {
            BackendKind::Rule
        } else {
            e
        };
        assert_eq!(d.backend, expected_backend);
    }
}

#[tokio::test]
async fn malformed_layer_keeps_last_valid_floor_and_stops() {
    let (service, calls) = chain(vec![
        (
            BackendKind::Rule,
            BackendOutcome::NoConclusion {
                floor: vec![n(BackendKind::Rule)],
            },
        ),
        (
            BackendKind::Encoder,
            BackendOutcome::NoConclusion { floor: vec![] },
        ),
        (
            BackendKind::Deep,
            BackendOutcome::Decided(vec![n(BackendKind::Deep), c(BackendKind::Deep)]),
        ),
        (
            BackendKind::Encoder,
            BackendOutcome::Decided(full(BackendKind::Encoder)),
        ),
    ]);
    let d = service.decide(&q()).await.unwrap();
    assert_eq!(
        d.outcome,
        invalid_response("answer_coverage", 2, BackendKind::Deep)
    );
    assert_eq!(d.answers, vec![n(BackendKind::Rule)]);
    assert_eq!(d.backend, BackendKind::Rule);
    assert_eq!(d.model, None);
    assert_eq!(calls[2].evaluate(), 1);
    assert_eq!(calls[3].evaluate(), 0);
    assert_eq!(calls[3].kind(), 0);
}

#[tokio::test]
async fn malformed_floor_is_discarded_whole_not_merged_or_salvaged() {
    let e = BackendKind::Encoder;
    let (service, calls) = chain(vec![
        (
            BackendKind::Rule,
            BackendOutcome::NoConclusion {
                floor: vec![n(BackendKind::Rule)],
            },
        ),
        (e, BackendOutcome::NoConclusion { floor: vec![s(e)] }),
        (
            e,
            BackendOutcome::NoConclusion {
                floor: vec![c(e), x(e)],
            },
        ),
        (
            BackendKind::Deep,
            BackendOutcome::Decided(full(BackendKind::Deep)),
        ),
    ]);
    let d = service.decide(&q()).await.unwrap();
    assert_eq!(d.outcome, invalid_response("unknown_question_id", 2, e));
    assert_eq!(
        d.answers,
        vec![s(e)],
        "keeps [S] only: no C salvaged, no N merged"
    );
    assert_eq!(d.backend, e);
    assert_eq!(calls[3].evaluate(), 0);
}

#[tokio::test]
async fn first_layer_malformed_without_floor() {
    let (service, calls) = chain(vec![
        (
            BackendKind::Encoder,
            BackendOutcome::Decided(vec![n(BackendKind::Encoder)]),
        ),
        (
            BackendKind::Deep,
            BackendOutcome::Decided(full(BackendKind::Deep)),
        ),
    ]);
    let d = service.decide(&q()).await.unwrap();
    assert!(d.answers.is_empty());
    assert_eq!(d.backend, BackendKind::Encoder);
    assert_eq!(
        d.outcome,
        invalid_response("answer_coverage", 0, BackendKind::Encoder)
    );
    assert_eq!(calls[0].evaluate(), 1);
    assert_eq!(calls[1].evaluate(), 0);
}

#[tokio::test]
async fn l1_unavailable_empty_floor_and_no_backend_paths_are_unchanged() {
    let d = run(
        &q(),
        vec![
            (
                BackendKind::Rule,
                BackendOutcome::NoConclusion {
                    floor: vec![n(BackendKind::Rule)],
                },
            ),
            (
                BackendKind::Encoder,
                BackendOutcome::Unavailable {
                    reason: "encoder unavailable".to_owned(),
                },
            ),
        ],
    )
    .await;
    assert_eq!(d.answers, vec![n(BackendKind::Rule)]);
    assert_eq!(d.backend, BackendKind::Rule);
    assert_eq!(
        d.outcome,
        Outcome::Unavailable {
            reason: "encoder unavailable".to_owned(),
            failed_stage_index: 1,
            failed_backend_kind: BackendKind::Encoder,
        }
    );

    let d = run(
        &q(),
        vec![(
            BackendKind::Encoder,
            BackendOutcome::NoConclusion { floor: vec![] },
        )],
    )
    .await;
    assert!(d.answers.is_empty(), "empty floor does not invent answers");
    assert!(matches!(d.outcome, Outcome::Abstain { .. }));

    let d = DecisionService::new(vec![]).decide(&q()).await.unwrap();
    assert!(d.answers.is_empty());
    assert_eq!(d.backend, BackendKind::Rule);
    assert_eq!(
        d.outcome,
        Outcome::Abstain {
            reason: "no backend reached a conclusion".to_owned()
        }
    );
}

#[tokio::test]
async fn multiple_errors_report_the_first_by_priority() {
    let e = BackendKind::Encoder;
    // unknown ID + p=NaN on that same answer => unknown_question_id
    let unknown_nan = vec![
        n(e),
        ans("x", AnswerValue::Bool(true), Some(f32::NAN), e, false),
    ];
    assert_response_rejected(e, unknown_nan, "unknown_question_id").await;
    // duplicate ID + wrong kind on the duplicate => duplicate_answer_id
    let dup_kind = vec![
        n(e),
        ans("n", AnswerValue::Label("yes".to_owned()), None, e, false),
    ];
    assert_response_rejected(e, dup_kind, "duplicate_answer_id").await;
    // missing coverage + invalid p => invalid_probability (coverage is last)
    let cov_p = vec![
        n(e),
        ans(
            "c",
            AnswerValue::Label("yes".to_owned()),
            Some(f32::NAN),
            e,
            true,
        ),
    ];
    assert_response_rejected_in(Mode::Decided, e, cov_p, "invalid_probability").await;
}

// ---------------------------------------------------------------- snapshots

fn snapshot(mut d: Decision) -> String {
    d.latency_ms = 0;
    serde_json::to_string(&d).unwrap()
}

#[tokio::test]
async fn serialized_snapshots_with_fixed_latency() {
    // Invalid request: serialize only the InvalidRequest inside Err.
    let mut empty = q();
    empty.questions.clear();
    let err = DecisionService::new(vec![])
        .decide(&empty)
        .await
        .unwrap_err();
    assert_eq!(
        serde_json::to_string(&err).unwrap(),
        r#"{"code":"empty_questions"}"#
    );

    // stage0 Rule floor N, stage1 Encoder malformed: full Decision fixture.
    let d = run(
        &q(),
        vec![
            (
                BackendKind::Rule,
                BackendOutcome::NoConclusion {
                    floor: vec![n(BackendKind::Rule)],
                },
            ),
            (
                BackendKind::Encoder,
                BackendOutcome::Decided(vec![n(BackendKind::Encoder)]),
            ),
        ],
    )
    .await;
    assert_eq!(
        snapshot(d),
        r#"{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":null,"calibrated":false}],"backend":"rule","model":null,"latency_ms":0,"outcome":{"kind":"invalid_response","code":"answer_coverage","failed_stage_index":1,"failed_backend_kind":"encoder"}}"#
    );

    // Existing outcomes keep their bytes.
    let decided = run(
        &q(),
        vec![(
            BackendKind::Encoder,
            BackendOutcome::Decided(full(BackendKind::Encoder)),
        )],
    )
    .await;
    assert_eq!(
        serde_json::to_string(&decided.outcome).unwrap(),
        r#"{"kind":"decided"}"#
    );
    assert_eq!(
        snapshot(decided),
        r#"{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":null,"calibrated":false},{"question_id":"c","value":{"type":"label","value":"yes"},"p":1.0,"calibrated":true},{"question_id":"s","value":{"type":"score","value":0.0},"p":0.0,"calibrated":false}],"backend":"encoder","model":null,"latency_ms":0,"outcome":{"kind":"decided"}}"#
    );

    let abstain = DecisionService::new(vec![]).decide(&q()).await.unwrap();
    assert_eq!(
        serde_json::to_string(&abstain.outcome).unwrap(),
        r#"{"kind":"abstain","reason":"no backend reached a conclusion"}"#
    );
    assert_eq!(
        snapshot(abstain),
        r#"{"answers":[],"backend":"rule","model":null,"latency_ms":0,"outcome":{"kind":"abstain","reason":"no backend reached a conclusion"}}"#
    );

    let unavailable = run(
        &q(),
        vec![
            (
                BackendKind::Rule,
                BackendOutcome::NoConclusion {
                    floor: vec![n(BackendKind::Rule)],
                },
            ),
            (
                BackendKind::Encoder,
                BackendOutcome::Unavailable {
                    reason: "encoder unavailable".to_owned(),
                },
            ),
        ],
    )
    .await;
    assert_eq!(
        serde_json::to_string(&unavailable.outcome).unwrap(),
        r#"{"kind":"unavailable","reason":"encoder unavailable","failed_stage_index":1,"failed_backend_kind":"encoder"}"#
    );
    assert_eq!(
        snapshot(unavailable),
        r#"{"answers":[{"question_id":"n","value":{"type":"bool","value":true},"p":null,"calibrated":false}],"backend":"rule","model":null,"latency_ms":0,"outcome":{"kind":"unavailable","reason":"encoder unavailable","failed_stage_index":1,"failed_backend_kind":"encoder"}}"#
    );
}
