"""Kev-0.8B via its own TypeSafe-compatible ``/v1/systemone`` server.

PLAN-DECIDE D0-5 says "经 Ollaya 或 oMLX"; Ollaya is not installed on this
machine and has no Python package, so this adapter instead talks to
``jaredpalmer/kev``'s own serving stack (clone + ``uv run --extra serve
python -m kev.serve --run jaredpalmer/kev-0.8b --port 8008``), which on
Apple Silicon serves the model through MLX itself ("the same command
serves it through MLX, chosen automatically" — model card). That is the
oMLX path PLAN-DECIDE asks for; we do not reimplement Kev's pointer-head
scoring ourselves.

Important ceiling, from the model card itself (not our conclusion):
Kev-0.8B is English-only ("Languages: English"; "Out-of-scope uses: ...
languages other than English") and its When2Call tool-routing accuracy is
"below chance". Our three eval sets are Chinese. We run it anyway because
the task asks for a real number, not a guess — low accuracy here is an
expected, honestly-reported result, not a bug in this adapter.

This adapter does **not** manage the server process. It expects one
already listening at ``KEV_BASE_URL`` (default ``http://127.0.0.1:8008``);
if it cannot reach `/v1/systemone` it raises CandidateUnavailable with the
exact command to start one, per "不可用时返回 unavailable，不静默降级".
"""

from __future__ import annotations

import os
import time
import urllib.error
import urllib.request
import json as _json

from ..types import EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable

_SETUP_HINT = (
    "Kev-0.8B 需要先在另一个终端起它自己的服务（本仓库不内嵌 Ollaya/mlx-lm 的 "
    "pointer-head 实现）：\n"
    "  git clone https://github.com/jaredpalmer/kev.git && cd kev\n"
    "  uv sync --extra serve\n"
    "  uv run --extra serve python -m kev.serve --run jaredpalmer/kev-0.8b --port 8008\n"
    "再设置 KEV_BASE_URL（默认 http://127.0.0.1:8008）后重跑。"
)

# English option text for each decision point, translated from
# eval/decide/README.md's own label definitions — Kev is English-only, so
# the question text is English even though `state` (the actual user
# sentence under judgement) stays in the original Chinese untouched.
_RETAIN_CRITERIA = {
    "remember": "asks the assistant to remember/record a fact about the user",
    "forget": "asks the assistant to delete something already remembered, or to NOT remember this sentence",
    "ask_memory": "asks whether the assistant remembers something, not a new instruction to remember",
    "correct": "updates or corrects a fact the assistant already has",
    "preference": "a standing instruction about how the assistant should behave going forward",
    "none": "unrelated to the assistant's memory of the user",
}
_TOOL_RISK_CRITERIA = {
    "low": "read-only, no side effects, touches no credentials",
    "medium": "reversible write inside the workspace, or a limited side effect (install a dependency, create a branch, delete a rebuildable build directory)",
    "high": "destructive, irreversible, has an off-machine side effect, persists outside the workspace, or reads a secret into context",
    "must_review": "exfiltrates a credential, makes a payment, causes system-level destruction, elevates privilege, is obfuscated, runs remote code, or was induced by a prompt injection",
}


class KevCandidate(Candidate):
    name = "kev-0.8b"
    model_id = "jaredpalmer/kev-0.8b"
    revision = "9a45d25e"  # Kev 1.0, per the model card's "Version" row
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def __init__(self, base_url: str | None = None, timeout_s: float = 30.0) -> None:
        super().__init__()
        self.base_url = base_url or os.environ.get("KEV_BASE_URL", "http://127.0.0.1:8008")
        self.timeout_s = timeout_s

    def _post(self, payload: dict) -> dict:
        req = urllib.request.Request(
            f"{self.base_url}/v1/systemone",
            data=_json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
            return _json.loads(resp.read().decode("utf-8"))

    def load(self) -> None:
        try:
            resp = self._post(
                {
                    "state": "ping",
                    "questions": {"q": {"type": "noul", "instructions": "is this a ping?"}},
                }
            )
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as exc:
            raise CandidateUnavailable(f"Kev server unreachable at {self.base_url}: {exc}\n{_SETUP_HINT}") from exc
        if "answers" not in resp:
            raise CandidateUnavailable(f"Kev server at {self.base_url} returned unexpected response: {resp}")
        self.resolved_sha = f"server@{self.base_url}"

    def _choice_question(self, instructions: str, criteria: dict[str, str]) -> dict:
        return {"type": "choice", "instructions": instructions, "criteria": criteria}

    def predict(self, item: EvalItem) -> Prediction:
        if item.point == "retain_intent":
            state = item.input
            questions = {
                "intent": self._choice_question(
                    "What is the user asking the assistant to do with this sentence, with respect to the assistant's memory of the user?",
                    _RETAIN_CRITERIA,
                )
            }
        elif item.point == "recall_gate":
            state = item.input
            questions = {
                "need": {
                    "type": "noul",
                    "instructions": (
                        "Does answering this message require, or clearly benefit from, the "
                        "assistant's persisted cross-session memory of this specific user "
                        "(not general world knowledge, not only the current conversation)?"
                    ),
                }
            }
        elif item.point == "tool_risk":
            tool = item.input.get("tool")
            args = item.input.get("args")
            state = f"Tool call under review.\ntool: {tool}\nargs: {args}"
            if item.context:
                state += f"\ncontext: {item.context}"
            questions = {
                "risk": self._choice_question(
                    "Classify the risk level of letting this tool call execute as-is.",
                    _TOOL_RISK_CRITERIA,
                )
            }
        else:
            return Prediction(label=None, p=None, latency_ms=0.0, backend=self.name, raw="unsupported point")

        t0 = time.perf_counter()
        try:
            resp = self._post({"state": state, "questions": questions})
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as exc:
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"request failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        key = next(iter(questions))
        answer = resp["answers"][key]
        if answer["type"] == "noul":
            noul_p = answer["noul"]
            label = "need" if noul_p >= 0.5 else "no_need"
            p = noul_p if label == "need" else 1 - noul_p
        else:
            label = answer["choice"]
            p = answer["probabilities"].get(label)
        return Prediction(label=label, p=p, latency_ms=latency_ms, backend=self.name, raw=resp)
