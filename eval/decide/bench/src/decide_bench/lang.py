"""Language tagging for the D0-8 三语分指标 requirement.

The published `eval/decide/*.jsonl` sets do not (yet) carry an explicit
``lang`` field — a separate task (`ab/decide-01`) is expanding them to
zh/en/th with one. Until that lands and this branch merges it, every item
here gets its language **inferred** by a simple, documented rule so the
bench can still report per-language numbers today:

    1. any Thai-script character present      -> "th"
    2. else any CJK Unified Ideograph present  -> "zh"
    3. else                                    -> "en"

This is a coarse heuristic (code-switched sentences, romanized Thai, or a
Chinese sentence quoting an English proper noun all fall through
imperfectly), not a real language identifier — it is only meant to bucket
the *current*, mostly-Chinese-with-some-English eval sets for reporting,
not to replace the real trilingual labels. Every report produced with
inferred tags says so explicitly (see ``report.py``); once `ab/decide-01`
merges, ``resolve_lang`` prefers the explicit ``item.lang`` field and only
falls back to inference where it is still absent.
"""

from __future__ import annotations

from .types import EvalItem

_THAI_RANGE = (0x0E00, 0x0E7F)
_CJK_RANGES = (
    (0x4E00, 0x9FFF),  # CJK Unified Ideographs
    (0x3400, 0x4DBF),  # CJK Extension A
    (0xF900, 0xFAFF),  # CJK Compatibility Ideographs
)


def _has_in_range(text: str, lo: int, hi: int) -> bool:
    return any(lo <= ord(ch) <= hi for ch in text)


def infer_lang(text: str) -> str:
    """Rule-based zh/en/th guess for a single string. See module docstring."""
    if not text:
        return "en"
    if _has_in_range(text, *_THAI_RANGE):
        return "th"
    if any(_has_in_range(text, lo, hi) for lo, hi in _CJK_RANGES):
        return "zh"
    return "en"


def _item_text(item: EvalItem) -> str:
    if item.point == "tool_risk":
        parts = [str(item.input.get("tool", "")), str(item.input.get("args", ""))]
        if item.context:
            parts.append(str(item.context))
        return " ".join(parts)
    return str(item.input)


def resolve_lang(item: EvalItem) -> tuple[str, bool]:
    """Returns ``(lang, was_inferred)``. Prefers an explicit ``item.lang``
    (from the eval JSONL, once the trilingual set lands); falls back to
    :func:`infer_lang` on the item's rendered text, flagging that it was
    inferred so reports can say so.
    """
    if item.lang:
        return item.lang, False
    return infer_lang(_item_text(item)), True
