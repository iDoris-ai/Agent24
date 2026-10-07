"""Faithful Python port of ``explicit_remember`` in
``rust/crates/agent24-agent/src/retain.rs`` @ ``origin/ab/m1-memory`` commit
``c503bf1`` (includes M1-T13).

Only meaningful for ``retain_intent`` (PLAN-DECIDE D0-5: "只对 retain_intent
有意义"); it is the floor the model candidates are measured against, and
the rest of ``eval/decide/README.md``'s ``rule_fn``/``rule_fp`` tags were
computed from it. The function below must be byte-for-byte equivalent to
the Rust source for every input in ``RETAIN_RS_TEST_TABLE`` — that table is
copied verbatim from ``retain.rs``'s own ``#[test]`` and is checked by
``tests/test_rule_port.py`` before this module is trusted for anything
else.

The Rust function returns ``Option<&str>`` (the extracted object text, or
None). For retain_intent's 6-way label this adapter only uses the
Some/None split: ``Some`` -> ``"remember"``, ``None`` -> ``"none"``. The
rule was never designed to tell forget/ask_memory/correct/preference/none
apart, so this is a known, documented ceiling, not a bug — see the bench
report for how that shows up in macro F1.
"""

from __future__ import annotations

import time

from ..types import EvalItem, Point, Prediction
from .base import Candidate

# --- verbatim port of retain.rs -------------------------------------------------

_ADDRESS_PREFIXES_CN = [
    "你帮我",
    "请帮我",
    "请你",
    "麻烦你",
    "帮我",
    "请",
    "麻烦",
    "你",
]
_REMEMBER_VERBS_CN = ["记一下", "记住", "记好", "记着"]
_SEPARATORS = " 　，,：:、"
_END_PUNCT = "。.！!"


def _is_question_sentence(text: str) -> bool:
    trimmed = text.strip()
    if trimmed.endswith(("吗", "么", "呢", "？", "?")):
        return True
    if trimmed.endswith(("了吗", "了没", "了没有", "没有")):
        return True
    if trimmed.startswith("了"):
        return True
    return False


def _strip_prefix_insensitive(s: str, prefix: str) -> str | None:
    if s.lower().startswith(prefix.lower()):
        return s[len(prefix) :]
    return None


def explicit_remember(prompt: str) -> str | None:
    prompt_trimmed = prompt.rstrip()
    if prompt_trimmed == "remember that":
        return None

    remaining = prompt

    for prefix in _ADDRESS_PREFIXES_CN:
        if remaining.startswith(prefix):
            remaining = remaining[len(prefix) :]
            break

    if remaining == prompt:
        lower = remaining.lower()
        if lower.startswith("please ") and lower[7:].startswith("remember that "):
            after_please = _strip_prefix_insensitive(remaining, "please ")
            if after_please is not None:
                remaining = after_please

    remaining = remaining.strip()

    for verb in _REMEMBER_VERBS_CN:
        if remaining.startswith(verb):
            after_verb = remaining[len(verb) :]
            if (
                after_verb.startswith("了")
                or after_verb.startswith("吗")
                or (after_verb.startswith("没") and not after_verb.startswith("没有"))
            ):
                continue
            body = after_verb.lstrip(_SEPARATORS)
            body = body.strip()
            if body:
                body = body.rstrip(_END_PUNCT)
                if body:
                    if _is_question_sentence(body):
                        return None
                    return body
            return None

    body = _strip_prefix_insensitive(remaining, "remember that ")
    if body is not None:
        body = body.strip()
        if body:
            body = body.rstrip(_END_PUNCT)
            if body:
                if _is_question_sentence(body):
                    return None
                return body
        return None

    body = _strip_prefix_insensitive(remaining, "remember ")
    if body is not None:
        body = body.strip()
        if body:
            body = body.rstrip(_END_PUNCT)
            if body:
                if _is_question_sentence(body):
                    return None
                return body
        return None

    return None


# --- the exact test table from retain.rs's `recognizes_only_the_supported_leading_forms` --

RETAIN_RS_TEST_TABLE: list[tuple[str, str | None]] = [
    ("记住我对花生过敏", "我对花生过敏"),
    ("请记住 我住在上海", "我住在上海"),
    ("remember my birthday is May 2", "my birthday is May 2"),
    ("remember that I prefer tea", "I prefer tea"),
    ("remember thatched roof", "thatched roof"),
    ("remember that", None),
    ("remember ", None),
    ("记住   ", None),
    ("please remember my birthday", None),
    ("I remember that I prefer tea", None),
    ("rememberable facts", None),
    ("你记住，我对花生过敏。", "我对花生过敏"),
    ("请你记住：我住在上海", "我住在上海"),
    ("帮我记一下 明天下午三点开会", "明天下午三点开会"),
    ("麻烦记住我不吃辣", "我不吃辣"),
    ("Please remember that my dog is Max", "my dog is Max"),
    ("记住没有人会来接你", "没有人会来接你"),
    ("你帮我记住我不吃香菜", "我不吃香菜"),
    ("请帮我记住下周二开会", "下周二开会"),
    ("你记住我吗", None),
    ("你记住这件事吗", None),
    ("你记住他的名字了吗", None),
    ("记住了没有", None),
    ("你记住了吗？", None),
    ("你还记得我对什么过敏吗", None),
    ("我记住了", None),
    ("记住了吗", None),
    ("你记住了没有", None),
    ("do you remember my name", None),
    ("remembered", None),
]


def verify_port() -> None:
    """Raise AssertionError on the first mismatch against retain.rs's own
    test table. Called by tests/test_rule_port.py and once at Candidate
    load time, so a silently-wrong port can never produce bench numbers."""
    for prompt, expected in RETAIN_RS_TEST_TABLE:
        got = explicit_remember(prompt)
        assert got == expected, f"rule port mismatch for {prompt!r}: expected {expected!r}, got {got!r}"


class RuleCandidate(Candidate):
    name = "rule"
    model_id = None
    revision = None
    applicable_points: tuple[Point, ...] = ("retain_intent",)

    def load(self) -> None:
        verify_port()
        self.resolved_sha = "retain.rs@ab/m1-memory:c503bf1242ed17bda05daaaed2ad01a5bcc248e6"

    def predict(self, item: EvalItem) -> Prediction:
        t0 = time.perf_counter()
        result = explicit_remember(item.input)
        latency_ms = (time.perf_counter() - t0) * 1000
        label = "remember" if result is not None else "none"
        return Prediction(label=label, p=None, latency_ms=latency_ms, backend="rule", raw=result)
