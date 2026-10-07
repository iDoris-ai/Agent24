"""近似重复检测：`train_data/*.jsonl` vs `eval/decide/*.jsonl`。

`adapters/setfit_bgem3.py::_load_train_rows` 已经做了逐字精确匹配的集合差
集检查——那一道只能挡住原样复制。PR #718 复审（clestons，2026-10-07，见
https://github.com/iDoris-ai/Agent24/pull/718#pullrequestreview-5440415310）
用 `difflib.SequenceMatcher` 另外跑了一遍，发现训练集里混了几条换模板词
的近义句（例如训练集「上海明天天气」对评测集「北京明天天气」，相似度
0.94），逐字检查查不出来。

这里补两道同一类检查的确定性实现（都做，不是二选一，原因见下）：

1. **字符 n-gram（默认 3-gram）Jaccard 相似度**——零额外依赖、确定性、
   可审计（两个集合的交并比，没有模型版本漂移的问题）。实测发现它对短
   中文句子（6–10 字）里换 1–2 个字的近义改写不够敏感：n-gram 集合里一
   个字的变化会带走该字参与的全部 n 个 gram，短句上这个比例很容易把
   Jaccard 压到 0.7 以下，即使人读起来明显是同一模板。它只稳定抓住了
   `tool_risk` 里那条最明显的命中（长模板里换一个城市名，共享前后缀很
   长）。
2. **`difflib.SequenceMatcher.ratio()`**——与 PR #718 复审（clestons）用的
   方法一致，标准库自带、零额外依赖；对短句的字符级编辑距离更敏感，复
   现了复审报告里的全部四个最高命中（0.80–0.94）。

两者都跑，命中判定是 **任一** 指标 ≥ 阈值（默认 0.7，取自复审实测的量
级）就算命中，`OverlapHit` 同时记录两个分数供复核；排序用两者的最大值。
之所以不只留 SequenceMatcher 更敏感的那个，是因为 n-gram Jaccard 对长
文本（比如未来加入更长的 `context` 字段）更稳健，两者互补。

两道都不是 embedding cosine——SetFit 本身要训练一次 bge-m3，再跑一次
embedding 做重复检测会让这个检查依赖模型下载/推理，与"零模型、随时可
跑"的检查定位冲突；n-gram + SequenceMatcher 已经覆盖了 PR #718 发现的
全部案例，暂不需要 embedding。
"""

from __future__ import annotations

import difflib
import json
from dataclasses import dataclass
from pathlib import Path

from .data import load_set
from .render import render_item_text
from .types import Point

# eval/decide/bench/src/decide_bench/overlap.py -> eval/decide/bench
_BENCH_DIR = Path(__file__).resolve().parents[2]
_TRAIN_DIR = _BENCH_DIR / "train_data"

_TRAIN_FILES: dict[Point, str] = {
    "retain_intent": "retain_intent_train.jsonl",
    "recall_gate": "recall_gate_train.jsonl",
    "tool_risk": "tool_risk_train.jsonl",
}

DEFAULT_THRESHOLD = 0.7
DEFAULT_NGRAM = 3


def char_ngrams(text: str, n: int = DEFAULT_NGRAM) -> set[str]:
    """Character n-grams of ``text``. Shorter-than-n strings fall back to
    the whole (stripped) string as a single "gram" so short items don't
    spuriously get an empty set."""
    text = text.strip()
    if len(text) < n:
        return {text} if text else set()
    return {text[i : i + n] for i in range(len(text) - n + 1)}


def jaccard(a: set[str], b: set[str]) -> float:
    if not a and not b:
        return 1.0
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def seq_ratio(a: str, b: str) -> float:
    return difflib.SequenceMatcher(None, a, b).ratio()


@dataclass(frozen=True)
class OverlapHit:
    point: Point
    eval_id: str
    eval_text: str
    train_label: str
    train_text: str
    ngram_jaccard: float
    seq_ratio: float

    @property
    def similarity(self) -> float:
        """Max of the two metrics — what the threshold and sort use."""
        return max(self.ngram_jaccard, self.seq_ratio)


def _load_train_rows(point: Point) -> list[dict]:
    path = _TRAIN_DIR / _TRAIN_FILES[point]
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def check_overlap(
    points: list[Point] | None = None,
    threshold: float = DEFAULT_THRESHOLD,
    n: int = DEFAULT_NGRAM,
) -> list[OverlapHit]:
    """Compare every eval item's rendered text (``render_item_text``, the
    same rendering every text adapter and the SetFit trainer use) against
    every train row's text for the same decision point, using both char
    n-gram Jaccard and ``difflib.SequenceMatcher`` ratio (see module
    docstring for why both). Returns every pair where **either** metric is
    at or above ``threshold``, sorted by the max of the two, descending
    (worst first).

    Pure data/stdlib — does not import ``setfit``/``torch``/etc., so it
    runs even on a machine that can't load any ML candidate.
    """
    points = points or list(_TRAIN_FILES.keys())
    hits: list[OverlapHit] = []
    for point in points:
        train_rows = _load_train_rows(point)
        train_info = [(r["text"], r.get("label", "?"), char_ngrams(r["text"], n)) for r in train_rows]
        for item in load_set(point):
            eval_text = render_item_text(item)
            eval_ngrams = char_ngrams(eval_text, n)
            for train_text, train_label, t_ngrams in train_info:
                ng = jaccard(eval_ngrams, t_ngrams)
                sr = seq_ratio(eval_text, train_text)
                if ng >= threshold or sr >= threshold:
                    hits.append(
                        OverlapHit(
                            point=point,
                            eval_id=item.id,
                            eval_text=eval_text,
                            train_label=train_label,
                            train_text=train_text,
                            ngram_jaccard=ng,
                            seq_ratio=sr,
                        )
                    )
    hits.sort(key=lambda h: -h.similarity)
    return hits


def render_overlap_report(hits: list[OverlapHit], threshold: float, n: int = DEFAULT_NGRAM) -> str:
    if not hits:
        return f"no train/eval overlap at char-{n}gram Jaccard >= {threshold} or SequenceMatcher ratio >= {threshold}\n"
    lines = [f"{len(hits)} hit(s) at char-{n}gram Jaccard >= {threshold} or SequenceMatcher ratio >= {threshold}:"]
    for h in hits:
        lines.append(
            f"  [{h.point}] ngram_jaccard={h.ngram_jaccard:.3f} seq_ratio={h.seq_ratio:.3f} "
            f"eval={h.eval_id} {h.eval_text!r} <-> train({h.train_label}) {h.train_text!r}"
        )
    return "\n".join(lines) + "\n"
