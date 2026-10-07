"""Load the three D0-4 评测集 from ``eval/decide/*.jsonl``."""

from __future__ import annotations

import json
from pathlib import Path

from .types import EvalItem, Point

# eval/decide/bench/src/decide_bench/data.py -> eval/decide/
EVAL_DIR = Path(__file__).resolve().parents[3]

FILES: dict[Point, str] = {
    "retain_intent": "retain_intent.jsonl",
    "recall_gate": "recall_gate.jsonl",
    "tool_risk": "tool_risk.jsonl",
}


def load_set(point: Point, eval_dir: Path = EVAL_DIR) -> list[EvalItem]:
    path = eval_dir / FILES[point]
    items: list[EvalItem] = []
    with path.open(encoding="utf-8") as f:
        for line_no, line in enumerate(f, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError as exc:
                raise ValueError(f"{path}:{line_no}: invalid JSON: {exc}") from exc
            items.append(EvalItem.from_json(d))
    ids = [it.id for it in items]
    assert len(set(ids)) == len(ids), f"{path}: duplicate id"
    return items


def load_sets(points: list[Point] | None = None, eval_dir: Path = EVAL_DIR) -> dict[Point, list[EvalItem]]:
    points = points or list(FILES.keys())
    return {p: load_set(p, eval_dir) for p in points}
