from decide_bench.metrics import (
    accuracy,
    cost_weighted_error_rate,
    expected_calibration_error,
    macro_f1,
    percentile,
    retain_intent_extra,
)


def test_accuracy_basic():
    assert accuracy(["a", "b", "c"], ["a", "b", "x"]) == 2 / 3


def test_accuracy_none_prediction_counts_as_wrong():
    assert accuracy(["a"], [None]) == 0.0


def test_accuracy_empty():
    assert accuracy([], []) is None


def test_macro_f1_perfect():
    labels = ["a", "b"]
    assert macro_f1(["a", "b", "a", "b"], ["a", "b", "a", "b"], labels) == 1.0


def test_macro_f1_all_wrong():
    labels = ["a", "b"]
    f1 = macro_f1(["a", "a", "b", "b"], ["b", "b", "a", "a"], labels)
    assert f1 == 0.0


def test_ece_perfectly_calibrated_bucket():
    # 10 items, all predicted with confidence 0.9, 9/10 correct -> |0.9-0.9|=0
    correct = [True] * 9 + [False]
    conf = [0.9] * 10
    assert abs(expected_calibration_error(correct, conf) - 0.0) < 1e-9


def test_ece_miscalibrated():
    # confidence 1.0 but only half correct -> ECE should be 0.5 for that bucket
    correct = [True, False]
    conf = [1.0, 1.0]
    assert abs(expected_calibration_error(correct, conf) - 0.5) < 1e-9


def test_cost_weighted_error_rate_all_correct():
    assert cost_weighted_error_rate(["a", "b"], ["a", "b"], ["high", "low"]) == 0.0


def test_cost_weighted_error_rate_weights_high_more():
    # one high-cost wrong vs one low-cost wrong should give different rates
    r_high = cost_weighted_error_rate(["a", "b"], ["x", "b"], ["high", "low"])
    r_low = cost_weighted_error_rate(["a", "b"], ["a", "x"], ["high", "low"])
    assert r_high > r_low


def test_retain_intent_extra():
    expected = ["remember", "remember", "none", "forget"]
    predicted = ["remember", "none", "remember", "forget"]
    extra = retain_intent_extra(expected, predicted)
    # 1 of 2 remember recalled
    assert extra.remember_recall == 0.5
    # 1 of 2 non-remember falsely predicted as remember
    assert extra.false_write_rate == 0.5


def test_percentile_basic():
    assert percentile([1, 2, 3, 4, 5], 50) == 3
    assert percentile([], 50) is None
