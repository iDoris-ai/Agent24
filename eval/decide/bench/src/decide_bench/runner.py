"""Runs one or more candidates over one or more eval sets and assembles
the metrics PLAN-DECIDE D0-5 asks for: accuracy, macro F1, ECE (10
buckets), cost-weighted error rate, P50/P95 latency, peak RSS, download
size; plus retain_intent's false-write-rate / remember-recall pair.

A failing candidate (``CandidateUnavailable`` from ``load()``, or any
other exception while loading) is recorded as unavailable with the reason
and the run continues with the rest — "单个候选失败不终止全局".
"""

from __future__ import annotations

import platform
import resource
import time
import traceback
from dataclasses import asdict, dataclass, field
from typing import Any

from .adapters.base import Candidate, CandidateUnavailable
from .lang import resolve_lang
from .metrics import (
    accuracy,
    cost_weighted_error_rate,
    expected_calibration_error,
    macro_f1,
    percentile,
    retain_intent_extra,
)
from .types import LABELS, EvalItem, Point, Prediction


def _peak_rss_mb() -> float:
    ru = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # ru_maxrss is bytes on macOS/BSD, KB on Linux.
    divisor = 1024 * 1024 if platform.system() == "Darwin" else 1024
    return ru / divisor


def _download_size_mb(model_id: str | None) -> float | None:
    if not model_id:
        return 0.0
    try:
        import huggingface_hub

        cache = huggingface_hub.scan_cache_dir()
        total = 0
        for repo in cache.repos:
            if repo.repo_id == model_id:
                total += repo.size_on_disk
        return total / (1024 * 1024) if total else None
    except Exception:  # noqa: BLE001
        return None


@dataclass
class PointResult:
    point: Point
    n_items: int
    n_abstained: int
    accuracy: float | None
    macro_f1: float | None
    ece: float | None
    cost_weighted_error_rate: float | None
    p50_latency_ms: float | None
    p95_latency_ms: float | None
    retain_intent_extra: dict[str, float | None] | None = None
    predictions: list[dict[str, Any]] = field(default_factory=list)
    #: D0-8 三语分指标: lang -> {n_items, accuracy, macro_f1, cost_weighted_error_rate}.
    #: lang comes from decide_bench.lang.resolve_lang (explicit item.lang if
    #: present, else inferred — see that module for the rule and caveats).
    by_lang: dict[str, dict[str, Any]] = field(default_factory=dict)
    #: True if ANY item in this point had its lang inferred rather than
    #: read from an explicit `lang` field — surfaced so reports can flag
    #: that the by_lang breakdown is partly/fully a guess.
    lang_inferred: bool = False


@dataclass
class CandidateResult:
    name: str
    model_id: str | None
    revision: str | None
    resolved_sha: str | None
    available: bool
    unavailable_reason: str | None
    load_latency_s: float | None
    peak_rss_mb: float | None
    download_size_mb: float | None
    #: D0-8 训练/推理分离: RSS increment measured right after load() returns
    #: (so, for a candidate that trains a head in load() — setfit-bge-m3,
    #: embed_head.py's family — this is "backbone load + training" combined;
    #: for everything else it's just "model load"). ``peak_rss_mb`` above
    #: stays the whole run's peak (load + every predict()); the gap between
    #: the two is this run's best approximation of "inference added this
    #: much on top of training", which for most candidates here is ~0 since
    #: inference reuses the already-resident backbone and adds no new
    #: allocations worth noting at this item count. See DECISION-MODELS.md
    #: §10 for which candidates this split is actually informative for.
    load_phase_rss_mb: float | None = None
    points: dict[str, PointResult] = field(default_factory=dict)
    overlap_warning: str | None = None


def run_point(candidate: Candidate, point: Point, items: list[EvalItem]) -> PointResult:
    expected: list[str] = []
    predicted: list[str | None] = []
    cost_levels: list[str] = []
    confidences: list[float] = []
    correct_flags: list[bool] = []
    latencies: list[float] = []
    n_abstained = 0
    preds_log: list[dict[str, Any]] = []
    langs: list[str] = []
    any_inferred = False

    for item in items:
        try:
            pred: Prediction = candidate.predict(item)
        except Exception as exc:  # noqa: BLE001 - one item's crash must not kill the run
            pred = Prediction(label=None, p=None, latency_ms=0.0, backend=candidate.name, raw=f"predict() raised: {exc}")

        item_lang, inferred = resolve_lang(item)
        any_inferred = any_inferred or inferred
        langs.append(item_lang)

        expected.append(item.expected)
        predicted.append(pred.label)
        cost_levels.append(item.cost_level)
        latencies.append(pred.latency_ms)
        if pred.label is None:
            n_abstained += 1
        if pred.p is not None:
            confidences.append(pred.p)
            correct_flags.append(pred.label == item.expected)
        preds_log.append(
            {
                "id": item.id,
                "expected": item.expected,
                "predicted": pred.label,
                "p": pred.p,
                "correct": pred.label == item.expected,
                "cost_level": item.cost_level,
                "lang": item_lang,
                "lang_inferred": inferred,
            }
        )

    ece = expected_calibration_error(correct_flags, confidences) if len(confidences) == len(expected) else None

    extra = None
    if point == "retain_intent":
        ri = retain_intent_extra(expected, predicted)
        extra = {"false_write_rate": ri.false_write_rate, "remember_recall": ri.remember_recall}

    by_lang: dict[str, dict[str, Any]] = {}
    for lang in sorted(set(langs)):
        idxs = [i for i, lg in enumerate(langs) if lg == lang]
        sub_expected = [expected[i] for i in idxs]
        sub_predicted = [predicted[i] for i in idxs]
        sub_cost = [cost_levels[i] for i in idxs]
        by_lang[lang] = {
            "n_items": len(idxs),
            "accuracy": accuracy(sub_expected, sub_predicted),
            "macro_f1": macro_f1(sub_expected, sub_predicted, LABELS[point]),
            "cost_weighted_error_rate": cost_weighted_error_rate(sub_expected, sub_predicted, sub_cost),
        }

    return PointResult(
        point=point,
        n_items=len(items),
        n_abstained=n_abstained,
        accuracy=accuracy(expected, predicted),
        macro_f1=macro_f1(expected, predicted, LABELS[point]),
        ece=ece,
        cost_weighted_error_rate=cost_weighted_error_rate(expected, predicted, cost_levels),
        p50_latency_ms=percentile(latencies, 50),
        p95_latency_ms=percentile(latencies, 95),
        retain_intent_extra=extra,
        predictions=preds_log,
        by_lang=by_lang,
        lang_inferred=any_inferred,
    )


def run_candidate(
    name: str, candidate: Candidate, items_by_point: dict[Point, list[EvalItem]], points: list[Point]
) -> CandidateResult:
    rss_before = _peak_rss_mb()
    t0 = time.perf_counter()
    try:
        candidate.load()
        load_latency = time.perf_counter() - t0
        load_phase_rss = _peak_rss_mb() - rss_before if rss_before else _peak_rss_mb()
    except CandidateUnavailable as exc:
        return CandidateResult(
            name=name,
            model_id=candidate.model_id,
            revision=candidate.revision,
            resolved_sha=None,
            available=False,
            unavailable_reason=str(exc),
            load_latency_s=None,
            peak_rss_mb=None,
            download_size_mb=None,
        )
    except Exception as exc:  # noqa: BLE001
        return CandidateResult(
            name=name,
            model_id=candidate.model_id,
            revision=candidate.revision,
            resolved_sha=None,
            available=False,
            unavailable_reason=f"load() raised unexpectedly: {exc}\n{traceback.format_exc()}",
            load_latency_s=None,
            peak_rss_mb=None,
            download_size_mb=None,
        )

    result = CandidateResult(
        name=name,
        model_id=candidate.model_id,
        revision=candidate.revision,
        resolved_sha=candidate.resolved_sha,
        available=True,
        unavailable_reason=None,
        load_latency_s=load_latency,
        peak_rss_mb=None,
        download_size_mb=_download_size_mb(candidate.model_id),
        overlap_warning=candidate.overlap_warning,
        load_phase_rss_mb=load_phase_rss,
    )

    for point in points:
        if not candidate.applicable(point):
            continue
        items = items_by_point.get(point, [])
        if not items:
            continue
        result.points[point] = run_point(candidate, point, items)

    result.peak_rss_mb = _peak_rss_mb() - rss_before if rss_before else _peak_rss_mb()
    return result


def result_to_dict(result: CandidateResult) -> dict[str, Any]:
    return asdict(result)
