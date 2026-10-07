"""Adapter protocol every candidate implements.

Every candidate wraps a model/runtime behind one method:
``predict(item) -> Prediction``. Model loading (and, for anything that
needs a download, the actual download) happens once in ``load()`` so the
runner can time and RSS-sample a candidate's warm state separately from
its cold-start cost.
"""

from __future__ import annotations

from abc import ABC, abstractmethod

from ..types import EvalItem, Point, Prediction


class CandidateUnavailable(Exception):
    """Raised by ``load()`` when a candidate cannot run on this machine.

    The runner catches this, records the candidate as ``unavailable`` with
    the message as the reason, and continues with the rest — per
    PLAN-DECIDE D0-5's "后端不可用时返回 unavailable，不静默降级" and the
    task's "单个候选失败不终止全局".
    """


class Candidate(ABC):
    #: short machine-readable id, used in --candidates filters and results.
    name: str
    #: HF repo id or other canonical model identifier, or None for rule.
    model_id: str | None = None
    #: pinned revision (HF commit) — must be set before any real run.
    revision: str | None = None
    #: which decision points this candidate is meaningful for. A candidate
    #: may be loadable but only "applicable" (see below) to a subset.
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def __init__(self) -> None:
        self._loaded = False
        self.resolved_sha: str | None = None
        #: set by a candidate's own load() when it trains on train_data and
        #: that data has near-duplicate overlap with the eval set it will be
        #: scored on (see decide_bench.overlap); None if not applicable or
        #: no overlap found. Surfaced in the report, never used to silently
        #: fix the data — see PLAN-DECIDE D0-6 / PR #718 review.
        self.overlap_warning: str | None = None

    def applicable(self, point: Point) -> bool:
        return point in self.applicable_points

    @abstractmethod
    def load(self) -> None:
        """Load weights/runtime. Raise CandidateUnavailable if it cannot
        run here (missing runtime, download failed, out of memory, ...).
        Must be idempotent."""

    @abstractmethod
    def predict(self, item: EvalItem) -> Prediction:
        """Run inference for one item. Must not raise for an ordinary
        low-confidence case — return a Prediction with label=None instead.
        May raise for a genuine per-item crash; the runner logs and skips."""

    def ensure_loaded(self) -> None:
        if not self._loaded:
            self.load()
            self._loaded = True
