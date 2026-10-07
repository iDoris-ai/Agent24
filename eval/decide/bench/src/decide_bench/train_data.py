"""Shared loader for ``train_data/*.jsonl`` (D0-4's hand-written few-shot
training rows), used by every candidate that trains a head on top of a
frozen embedding/encoder — originally only ``setfit_bgem3.py``, now also
D0-8's generic embedding+LogisticRegression candidates
(``adapters/embed_head.py``).

Kept in one place so the train/eval disjointness assertion (exact-text
match) only has one implementation to trust, matching the project's
"验证判据先被验过" habit — a second copy-pasted assert could silently
drift from this one.
"""

from __future__ import annotations

import json
from pathlib import Path

from .types import Point

# eval/decide/bench/src/decide_bench/train_data.py -> eval/decide/bench
_BENCH_DIR = Path(__file__).resolve().parents[2]
TRAIN_DIR = _BENCH_DIR / "train_data"

TRAIN_FILES: dict[Point, str] = {
    "retain_intent": "retain_intent_train.jsonl",
    "recall_gate": "recall_gate_train.jsonl",
    "tool_risk": "tool_risk_train.jsonl",
}


def load_train_rows(point: Point) -> list[dict]:
    """Rows of ``{"text": ..., "label": ...}`` for ``point``. Raises
    ``AssertionError`` if any row's exact text also appears in the
    published eval set for that point (train_data must stay disjoint)."""
    from .render import render_item_text

    path = TRAIN_DIR / TRAIN_FILES[point]
    rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    eval_texts = set()
    from .data import load_set

    for item in load_set(point):
        eval_texts.add(render_item_text(item))
    overlap = eval_texts & {r["text"] for r in rows}
    if overlap:
        raise AssertionError(f"train/eval overlap for {point}: {overlap!r} — train_data must stay disjoint from eval")
    return rows
