"""Metrics for the D0-5 横评：准确率、宏 F1、ECE（10 桶）、按 cost_level
加权误判率、以及 retain_intent 专用的误写入率/记住召回。

All functions take parallel lists (one entry per eval item) so a caller can
first drop items the adapter abstained on when computing accuracy/F1/ECE
("算分前先剔除弃权"), while still being able to report the abstain rate
itself.
"""

from __future__ import annotations

from dataclasses import dataclass

from .types import COST_WEIGHTS


def accuracy(expected: list[str], predicted: list[str | None]) -> float | None:
    if not expected:
        return None
    correct = sum(1 for e, p in zip(expected, predicted) if p is not None and e == p)
    return correct / len(expected)


def macro_f1(expected: list[str], predicted: list[str | None], labels: list[str]) -> float | None:
    """Macro-averaged F1 over ``labels``. A ``None`` prediction counts as a
    miss for whatever the expected label was (never a match), matching how
    an abstention looks to a downstream caller that must still act.
    """
    if not expected:
        return None
    f1s = []
    for lab in labels:
        tp = sum(1 for e, p in zip(expected, predicted) if p == lab and e == lab)
        fp = sum(1 for e, p in zip(expected, predicted) if p == lab and e != lab)
        fn = sum(1 for e, p in zip(expected, predicted) if p != lab and e == lab)
        if tp == 0 and fp == 0 and fn == 0:
            continue  # label absent from both expected and predicted on this set
        precision = tp / (tp + fp) if (tp + fp) else 0.0
        recall = tp / (tp + fn) if (tp + fn) else 0.0
        f1 = 2 * precision * recall / (precision + recall) if (precision + recall) else 0.0
        f1s.append(f1)
    if not f1s:
        return None
    return sum(f1s) / len(f1s)


def expected_calibration_error(
    correct: list[bool], confidence: list[float], n_bins: int = 10
) -> float | None:
    """Standard 10-bucket ECE: mean |accuracy(bin) - avg_confidence(bin)|,
    weighted by bin size over the whole set. Requires a confidence
    (``p`` for the predicted label) for every item — callers should only
    pass items from backends that actually emit a probability.
    """
    n = len(correct)
    if n == 0:
        return None
    bins = [[] for _ in range(n_bins)]
    for c, conf in zip(correct, confidence):
        conf = min(max(conf, 0.0), 1.0)
        idx = min(int(conf * n_bins), n_bins - 1)
        bins[idx].append((c, conf))
    ece = 0.0
    for bucket in bins:
        if not bucket:
            continue
        bucket_acc = sum(1 for c, _ in bucket if c) / len(bucket)
        bucket_conf = sum(conf for _, conf in bucket) / len(bucket)
        ece += (len(bucket) / n) * abs(bucket_acc - bucket_conf)
    return ece


def cost_weighted_error_rate(
    expected: list[str], predicted: list[str | None], cost_level: list[str]
) -> float | None:
    """sum(weight for each wrong/abstained item) / sum(weight for all items).

    Weights are the constants in ``types.COST_WEIGHTS`` (high=5, medium=2,
    low=1), per ``eval/decide/README.md``'s "cost_level 含义" section. An
    abstention on a high-cost item is scored exactly like a wrong answer —
    both leave the caller without the judgement it needed.
    """
    if not expected:
        return None
    total = 0.0
    wrong = 0.0
    for e, p, cl in zip(expected, predicted, cost_level):
        w = COST_WEIGHTS[cl]
        total += w
        if p != e:
            wrong += w
    return wrong / total if total else None


@dataclass
class RetainIntentExtra:
    """retain_intent 专用：误写入率（非 remember 被判成 remember）与记住召回。"""

    false_write_rate: float | None  # among expected != remember, how many predicted == remember
    remember_recall: float | None  # among expected == remember, how many predicted == remember


def retain_intent_extra(expected: list[str], predicted: list[str | None]) -> RetainIntentExtra:
    non_remember = [(e, p) for e, p in zip(expected, predicted) if e != "remember"]
    remember = [(e, p) for e, p in zip(expected, predicted) if e == "remember"]
    fwr = (
        sum(1 for _, p in non_remember if p == "remember") / len(non_remember)
        if non_remember
        else None
    )
    rec = sum(1 for _, p in remember if p == "remember") / len(remember) if remember else None
    return RetainIntentExtra(false_write_rate=fwr, remember_recall=rec)


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    k = (len(s) - 1) * (pct / 100.0)
    f = int(k)
    c = min(f + 1, len(s) - 1)
    if f == c:
        return s[f]
    return s[f] + (s[c] - s[f]) * (k - f)
