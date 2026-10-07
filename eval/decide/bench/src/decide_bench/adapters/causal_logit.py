"""D0-8 candidate D: Qwen3 causal LLM read via first-token logits
("LLM2Jev 式，零训练" in the task brief) — no fine-tuning, no generation
loop. One forward pass per item: the prompt lists the candidate labels as
lettered options (A/B/C/...), and we read the model's logits for just
those letter tokens at the next position, softmax-normalize over that
small candidate set (not the full vocabulary), and take the argmax. This
is the standard "LM as a zero-shot classifier via logprobs" technique —
the task mentions constrained decoding/logprob reading as the earlier
`#7.2` "小 LLM + 约束解码 + logprob" route, not the default one, but asks
for a real number here as one of five families to compare, not a
production pick.

Runtime: MLX (`mlx_lm`), per PLAN-DECIDE's oMLX preference on Apple
Silicon and per the task's explicit "优先 MLX，记录用哪个" instruction.
0.6B and 8B reuse the already-downloaded quantized weights at
``~/.omlx/models/`` (no new download, see `download_size_mb=0` in the
report); 1.7B and 4B pull ``mlx-community``'s pre-quantized 4-bit weights
(new download, within the 60GB budget — see PR body for the ledger).

No instruction/chat template is applied — these are base causal LMs, and
using a plain completion-style prompt keeps the "zero training" claim
literal (a hand-tuned chat template would itself be a form of prompt
engineering specific to one model family, working against the "同系列
尺寸连贯" comparison this is meant to support).
"""

from __future__ import annotations

import time
from pathlib import Path

from ..render import render_item_text
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable
from .gliclass import _LABEL_DESC  # same canonical Chinese label descriptions GLiClass/NLI use

_LETTERS = ["A", "B", "C", "D", "E", "F"]

_OMLX_LOCAL_PATHS = {
    "0.6b": Path.home() / ".omlx" / "models" / "Qwen3-0.6B-4bit",
    "8b": Path.home() / ".omlx" / "models" / "Qwen3-8B-4bit",
}


class Qwen3CausalLogitCandidate(Candidate):
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def __init__(self, size: str, model_id: str, revision: str | None, path_or_repo: str, reused_local: bool) -> None:
        super().__init__()
        self.size = size
        self.name = f"qwen3-llm-{size}"
        self.model_id = model_id
        self.revision = revision
        self._path_or_repo = path_or_repo
        # True when this reuses an already-present ~/.omlx/models/ copy —
        # surfaced so the report's download_size_mb can honestly show 0
        # instead of double-counting a model the user already had.
        self.reused_local = reused_local

    def load(self) -> None:
        try:
            import mlx.core as mx  # noqa: F401
            from mlx_lm.utils import load as mlx_load
        except ImportError as exc:
            raise CandidateUnavailable(
                f"mlx/mlx-lm not importable (install with `uv sync --extra mlx`): {exc}"
            ) from exc
        try:
            self._model, self._tokenizer = mlx_load(self._path_or_repo, revision=self.revision)
        except Exception as exc:  # noqa: BLE001
            raise CandidateUnavailable(f"failed to load {self._path_or_repo}@{self.revision}: {exc}") from exc

        try:
            self._letter_ids = [self._tokenizer.encode(l)[0] for l in _LETTERS]
        except Exception as exc:  # noqa: BLE001
            raise CandidateUnavailable(f"tokenizer could not encode option letters: {exc}") from exc

        if self.reused_local:
            self.resolved_sha = f"local:{self._path_or_repo}"
        else:
            try:
                import huggingface_hub

                info = huggingface_hub.model_info(self.model_id, revision=self.revision)
                self.resolved_sha = info.sha
            except Exception:  # noqa: BLE001
                self.resolved_sha = self.revision

    def _prompt(self, item: EvalItem) -> tuple[str, list[str]]:
        point = item.point
        labels = LABELS[point]
        desc = _LABEL_DESC[point]
        opts = "\n".join(f"{letter}. {desc[lab]}" for letter, lab in zip(_LETTERS, labels))
        text = render_item_text(item)
        prompt = (
            "判断下面这句话属于哪一类。\n"
            f"句子：{text}\n"
            "选项：\n"
            f"{opts}\n"
            "只回答一个字母，不要解释。\n"
            "答案："
        )
        return prompt, labels

    def predict(self, item: EvalItem) -> Prediction:
        import mlx.core as mx

        prompt, labels = self._prompt(item)
        t0 = time.perf_counter()
        try:
            ids = self._tokenizer.encode(prompt)
            x = mx.array(ids)[None]
            out = self._model(x)
            logits = out[0, -1]
            n = len(labels)
            sub = mx.array([logits[self._letter_ids[i]] for i in range(n)])
            probs = mx.softmax(sub, axis=-1).tolist()
        except Exception as exc:  # noqa: BLE001
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"inference failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        best_idx = max(range(len(probs)), key=lambda i: probs[i])
        return Prediction(
            label=labels[best_idx], p=float(probs[best_idx]), latency_ms=latency_ms, backend=self.name, raw=probs
        )


def make_qwen3_llm(size: str) -> Qwen3CausalLogitCandidate:
    if size in _OMLX_LOCAL_PATHS:
        local_path = _OMLX_LOCAL_PATHS[size]
        if local_path.exists():
            return Qwen3CausalLogitCandidate(
                size=size,
                model_id=f"mlx-community/Qwen3-{size.upper()}-4bit (reused from ~/.omlx/models/)",
                revision=None,
                path_or_repo=str(local_path),
                reused_local=True,
            )
    repo_revisions = {
        "1.7b": ("mlx-community/Qwen3-1.7B-4bit", "3b1b1768f8f8cf8351c712464f906e86c2b8269e"),
        "4b": ("mlx-community/Qwen3-4B-4bit", "4dcb3d101c2a062e5c1d4bb173588c54ea6c4d25"),
        "0.6b": ("mlx-community/Qwen3-0.6B-4bit", None),
        "8b": ("mlx-community/Qwen3-8B-4bit", None),
    }
    model_id, revision = repo_revisions[size]
    return Qwen3CausalLogitCandidate(
        size=size, model_id=model_id, revision=revision, path_or_repo=model_id, reused_local=False
    )
