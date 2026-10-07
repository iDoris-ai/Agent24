"""GLiClass-multilang (mini): zero-shot multi-task sequence classifier.

DECISION-MODELS.md §8.1: `knowledgator/gliclass-multilang-mini`, Apache-2.0,
kept for production. Base `microsoft/mdeberta-v3-base` (MIT). Natively
covers Chinese (one of its 20 trained languages).

Applies to all three eval sets as a zero-shot classifier: give it the
item's label set as classes (with short natural-language descriptions
from eval/decide/README.md's own 标签定义), take the top-scoring label.
"""

from __future__ import annotations

import time

from ..render import render_item_text
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable

_REVISION = "0bd888b6c3ef9fca5f0a9d407bddfbbc7623486b"  # pinned 2026-10-07, HEAD of main

_LABEL_DESC: dict[Point, dict[str, str]] = {
    "retain_intent": {
        "remember": "要求助手记住一条关于用户的新事实",
        "forget": "要求助手删除已记内容，或不要记住这句话",
        "ask_memory": "询问助手是否记得 / 记得什么，不是新的记住指令",
        "correct": "更新或更正一条已有的事实",
        "preference": "关于助手今后怎么做的持续性指令",
        "none": "与助手的记忆操作无关",
    },
    "recall_gate": {
        "need": "回答这句话需要用户的跨会话个人记忆",
        "no_need": "回答这句话不需要用户的个人记忆（寒暄/通用知识/仅靠本会话上下文/来源不是主人）",
    },
    "tool_risk": {
        "low": "只读、无副作用、不涉凭据",
        "medium": "工作区内可逆写入，或副作用有限",
        "high": "破坏性、不可撤回、离机副作用，或把秘密读进上下文",
        "must_review": "凭据外发、付款、系统级破坏、提权、混淆执行、远程代码，或由提示注入诱导",
    },
}


class GliClassCandidate(Candidate):
    name = "gliclass-multilang"
    model_id = "knowledgator/gliclass-multilang-mini"
    revision = _REVISION
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def load(self) -> None:
        try:
            import torch
            from gliclass import GLiClassModel, ZeroShotClassificationPipeline
            from transformers import AutoTokenizer
        except ImportError as exc:
            raise CandidateUnavailable(f"gliclass/transformers/torch not importable: {exc}") from exc

        try:
            self._model = GLiClassModel.from_pretrained(self.model_id, revision=self.revision)
            self._tokenizer = AutoTokenizer.from_pretrained(self.model_id, revision=self.revision)
        except Exception as exc:  # noqa: BLE001 - any download/load failure means unavailable
            raise CandidateUnavailable(f"failed to load {self.model_id}@{self.revision}: {exc}") from exc

        device = "mps" if torch.backends.mps.is_available() else "cpu"
        self._pipeline = ZeroShotClassificationPipeline(
            self._model, self._tokenizer, classification_type="multi-label", device=device
        )
        try:
            import huggingface_hub

            info = huggingface_hub.model_info(self.model_id, revision=self.revision)
            self.resolved_sha = info.sha
        except Exception:  # noqa: BLE001 - best effort, not fatal
            self.resolved_sha = self.revision

    def predict(self, item: EvalItem) -> Prediction:
        point = item.point
        desc = _LABEL_DESC[point]
        labels = LABELS[point]
        label_texts = [f"{lab}: {desc[lab]}" for lab in labels]

        text = render_item_text(item)

        t0 = time.perf_counter()
        try:
            results = self._pipeline(text, label_texts, threshold=0.0)[0]
        except Exception as exc:  # noqa: BLE001
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"inference failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        best = max(results, key=lambda r: r["score"])
        best_label = labels[label_texts.index(best["label"])]
        return Prediction(label=best_label, p=float(best["score"]), latency_ms=latency_ms, backend=self.name, raw=results)
