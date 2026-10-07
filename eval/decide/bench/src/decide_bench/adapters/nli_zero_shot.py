"""Shared zero-shot-via-NLI adapter, used for both mDeBERTa-xnli (control
only — DECISION-MODELS.md §8.1 excludes it from production candidates
because its XNLI training data is CC-BY-NC-4.0) and Erlangshen-NLI
(production candidate for the Chinese-NLI route).

Both are standard 3-way (entailment/neutral/contradiction) NLI heads and
load straight into `transformers.pipeline("zero-shot-classification", ...)`
— the same mechanism `facebook/bart-large-mnli` uses, just with a
multilingual/Chinese backbone. The premise is the item text; each
candidate label becomes a hypothesis via a Chinese template built from
eval/decide/README.md's own label definitions, so it's judged on the same
label semantics GLiClass gets, not a differently-worded proxy.
"""

from __future__ import annotations

import time

from ..render import render_item_text
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable
from .gliclass import _LABEL_DESC  # reuse the one canonical set of label descriptions

_HYPOTHESIS_TEMPLATE = "这句话属于：{}"


class NliZeroShotCandidate(Candidate):
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def __init__(self, name: str, model_id: str, revision: str) -> None:
        super().__init__()
        self.name = name
        self.model_id = model_id
        self.revision = revision

    def load(self) -> None:
        try:
            import torch
            from transformers import pipeline
        except ImportError as exc:
            raise CandidateUnavailable(f"transformers/torch not importable: {exc}") from exc
        try:
            device = 0 if torch.backends.mps.is_available() else -1
            self._pipe = pipeline(
                "zero-shot-classification",
                model=self.model_id,
                revision=self.revision,
                device="mps" if device == 0 else -1,
            )
        except Exception as exc:  # noqa: BLE001
            raise CandidateUnavailable(f"failed to load {self.model_id}@{self.revision}: {exc}") from exc
        try:
            import huggingface_hub

            info = huggingface_hub.model_info(self.model_id, revision=self.revision)
            self.resolved_sha = info.sha
        except Exception:  # noqa: BLE001
            self.resolved_sha = self.revision

    def predict(self, item: EvalItem) -> Prediction:
        point = item.point
        labels = LABELS[point]
        desc = _LABEL_DESC[point]
        candidate_labels = [desc[lab] for lab in labels]
        label_by_text = dict(zip(candidate_labels, labels))

        text = render_item_text(item)

        t0 = time.perf_counter()
        try:
            result = self._pipe(text, candidate_labels, hypothesis_template=_HYPOTHESIS_TEMPLATE)
        except Exception as exc:  # noqa: BLE001
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"inference failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        best_text = result["labels"][0]
        best_score = result["scores"][0]
        return Prediction(
            label=label_by_text[best_text], p=float(best_score), latency_ms=latency_ms, backend=self.name, raw=result
        )


def make_mdeberta_xnli() -> NliZeroShotCandidate:
    # DECISION-MODELS.md §8.1: control only, not a production candidate
    # (XNLI training data is CC-BY-NC-4.0).
    return NliZeroShotCandidate(
        name="mdeberta-xnli-CONTROL",
        model_id="MoritzLaurer/mDeBERTa-v3-base-mnli-xnli",
        revision="8adb042d524ecd5c26d3e3ba0e3fbcf7e2d0864c",
    )


def make_erlangshen_nli() -> NliZeroShotCandidate:
    return NliZeroShotCandidate(
        name="erlangshen-nli",
        model_id="IDEA-CCNL/Erlangshen-Roberta-110M-NLI",
        revision="864d25be3ce5e90d9193cb49d9fbd52722cdc6b0",
    )
