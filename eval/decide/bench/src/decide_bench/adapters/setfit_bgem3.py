"""SetFit head on top of BAAI/bge-m3, one small classifier per decision
point, few-shot trained on ``train_data/*.jsonl`` (hand-written, disjoint
from ``eval/decide/*.jsonl`` — see ``train_data/README.md``).

Train/eval split: training happens once in ``load()`` from the files in
``train_data/``; every number this bench reports for this candidate is
measured on the published `eval/decide/*.jsonl` sets, which the training
code never reads.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

from ..render import render_item_text
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable

_REVISION = "5617a9f61b028005a4858fdac845db406aefb181"  # BAAI/bge-m3, pinned 2026-10-07

# eval/decide/bench/src/decide_bench/adapters/setfit_bgem3.py -> eval/decide/bench
_BENCH_DIR = Path(__file__).resolve().parents[3]
_TRAIN_DIR = _BENCH_DIR / "train_data"

_TRAIN_FILES: dict[Point, str] = {
    "retain_intent": "retain_intent_train.jsonl",
    "recall_gate": "recall_gate_train.jsonl",
    "tool_risk": "tool_risk_train.jsonl",
}


def _load_train_rows(point: Point) -> list[dict]:
    path = _TRAIN_DIR / _TRAIN_FILES[point]
    rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    eval_texts = set()
    from ..data import load_set

    for item in load_set(point):
        eval_texts.add(render_item_text(item))
    overlap = eval_texts & {r["text"] for r in rows}
    if overlap:
        raise AssertionError(f"train/eval overlap for {point}: {overlap!r} — train_data must stay disjoint from eval")
    return rows


class SetFitBgeM3Candidate(Candidate):
    name = "setfit-bge-m3"
    model_id = "BAAI/bge-m3"
    revision = _REVISION
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def load(self) -> None:
        try:
            from setfit import SetFitModel, Trainer, TrainingArguments
            from datasets import Dataset
        except ImportError as exc:
            raise CandidateUnavailable(f"setfit/datasets not importable: {exc}") from exc

        self._models: dict[Point, object] = {}
        self.train_counts: dict[Point, int] = {}
        for point in self.applicable_points:
            try:
                rows = _load_train_rows(point)
            except AssertionError:
                raise
            except Exception as exc:  # noqa: BLE001
                raise CandidateUnavailable(f"could not read train_data for {point}: {exc}") from exc

            labels = LABELS[point]
            label2id = {lab: i for i, lab in enumerate(labels)}
            texts = [r["text"] for r in rows]
            ys = [label2id[r["label"]] for r in rows]
            self.train_counts[point] = len(rows)

            try:
                model = SetFitModel.from_pretrained(self.model_id, revision=self.revision, labels=labels)
                ds = Dataset.from_dict({"text": texts, "label": ys})
                args = TrainingArguments(num_epochs=1, batch_size=16, num_iterations=5, show_progress_bar=False)
                trainer = Trainer(model=model, args=args, train_dataset=ds)
                trainer.train()
            except Exception as exc:  # noqa: BLE001
                raise CandidateUnavailable(f"failed to train SetFit head for {point} on {self.model_id}: {exc}") from exc
            self._models[point] = model

        try:
            import huggingface_hub

            info = huggingface_hub.model_info(self.model_id, revision=self.revision)
            self.resolved_sha = info.sha
        except Exception:  # noqa: BLE001
            self.resolved_sha = self.revision

    def predict(self, item: EvalItem) -> Prediction:
        point = item.point
        model = self._models[point]
        text = render_item_text(item)
        labels = LABELS[point]

        t0 = time.perf_counter()
        try:
            probs = model.predict_proba([text])[0]
        except Exception as exc:  # noqa: BLE001
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"inference failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        probs = [float(p) for p in probs]
        best_idx = max(range(len(probs)), key=lambda i: probs[i])
        return Prediction(
            label=labels[best_idx], p=probs[best_idx], latency_ms=latency_ms, backend=self.name, raw=probs
        )
