"""SetFit head on top of BAAI/bge-m3, one small classifier per decision
point, few-shot trained on ``train_data/*.jsonl`` (hand-written, disjoint
from ``eval/decide/*.jsonl`` — see ``train_data/README.md``).

Train/eval split: training happens once in ``load()`` from the files in
``train_data/``; every number this bench reports for this candidate is
measured on the published `eval/decide/*.jsonl` sets, which the training
code never reads.
"""

from __future__ import annotations

import time

from ..overlap import check_overlap, render_overlap_report
from ..render import render_item_text
from ..train_data import load_train_rows as _load_train_rows
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable

_REVISION = "5617a9f61b028005a4858fdac845db406aefb181"  # BAAI/bge-m3, pinned 2026-10-07


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

        # D0-6: besides the exact-text assert in _load_train_rows below,
        # check for near-duplicate (char n-gram Jaccard >= 0.7) templates
        # between train_data and eval — this is reported, never used to
        # silently alter either file. See decide_bench.overlap docstring
        # and PR #718's review for why this exists.
        overlap_hits = check_overlap(points=list(self.applicable_points))
        self.overlap_warning = (
            render_overlap_report(overlap_hits, threshold=0.7).strip() if overlap_hits else None
        )

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
