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
    points: dict[str, PointResult] = field(default_factory=dict)


def run_point(candidate: Candidate, point: Point, items: list[EvalItem]) -> PointResult:
    expected: list[str] = []
    predicted: list[str | None] = []
    cost_levels: list[str] = []
    confidences: list[float] = []
    correct_flags: list[bool] = []
    latencies: list[float] = []
    n_abstained = 0
    preds_log: list[dict[str, Any]] = []

    for item in items:
        try:
            pred: Prediction = candidate.predict(item)
        except Exception as exc:  # noqa: BLE001 - one item's crash must not kill the run
            pred = Prediction(label=None, p=None, latency_ms=0.0, backend=candidate.name, raw=f"predict() raised: {exc}")

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
            }
        )

    ece = expected_calibration_error(correct_flags, confidences) if len(confidences) == len(expected) else None

    extra = None
    if point == "retain_intent":
        ri = retain_intent_extra(expected, predicted)
        extra = {"false_write_rate": ri.false_write_rate, "remember_recall": ri.remember_recall}

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
    )


def run_candidate(
    name: str, candidate: Candidate, items_by_point: dict[Point, list[EvalItem]], points: list[Point]
) -> CandidateResult:
    rss_before = _peak_rss_mb()
    t0 = time.perf_counter()
    try:
        candidate.load()
        load_latency = time.perf_counter() - t0
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
