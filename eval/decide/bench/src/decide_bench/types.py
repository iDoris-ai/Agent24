"""Shared data types for the D0-5 bench.

Schema mirrors ``eval/decide/README.md`` exactly — this module does not
invent fields, it only gives them a Python shape.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

Point = Literal["retain_intent", "recall_gate", "tool_risk"]

# Label universes per decision point (from eval/decide/README.md "标签定义").
LABELS: dict[Point, list[str]] = {
    "retain_intent": ["remember", "forget", "ask_memory", "correct", "preference", "none"],
    "recall_gate": ["need", "no_need"],
    "tool_risk": ["low", "medium", "high", "must_review"],
}

# Question shape per PLAN-DECIDE §7.3 / the Jev-shaped interface agent24-decide
# borrows: retain_intent is a 6-way choice, recall_gate is a yes/no "noul",
# tool_risk is a 4-level choice.
QUESTION_KIND: dict[Point, str] = {
    "retain_intent": "choice",
    "recall_gate": "noul",
    "tool_risk": "choice",
}

COST_WEIGHTS: dict[str, float] = {"high": 5.0, "medium": 2.0, "low": 1.0}
"""建议权重，README §cost_level 含义小节："high=5、medium=2、low=1，D0-5 可调"。"""


@dataclass(frozen=True)
class EvalItem:
    id: str
    point: Point
    input: Any  # str for retain_intent/recall_gate; {"tool":..., "args":...} for tool_risk
    expected: str
    cost_level: str
    tags: list[str] = field(default_factory=list)
    context: Any | None = None
    note: str | None = None
    #: explicit zh/en/th tag, once `ab/decide-01`'s trilingual eval set
    #: lands; None on today's sets, where decide_bench.lang.resolve_lang
    #: falls back to inferring it (see that module's docstring).
    lang: str | None = None

    @staticmethod
    def from_json(d: dict[str, Any]) -> "EvalItem":
        return EvalItem(
            id=d["id"],
            point=d["point"],
            input=d["input"],
            expected=d["expected"],
            cost_level=d["cost_level"],
            tags=list(d.get("tags", [])),
            context=d.get("context"),
            note=d.get("note"),
            lang=d.get("lang"),
        )


@dataclass
class Prediction:
    """What one adapter returns for one item.

    ``label`` is the adapter's best guess mapped into the item's label
    universe (``LABELS[point]``), or ``None`` if the adapter abstained /
    could not answer (e.g. a question type it does not support, or a
    partial failure on one item while the candidate overall is available).
    ``p`` is the model's own calibrated-or-not probability for that label,
    when the backend produces one; ``None`` if not applicable (e.g. pure
    rule backend).
    """

    label: str | None
    p: float | None
    latency_ms: float
    backend: str
    raw: Any = None
