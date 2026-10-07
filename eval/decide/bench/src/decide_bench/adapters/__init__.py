"""Candidate adapters for the D0-5 横评.

Each module exposes one ``Candidate`` subclass (or a factory, for the two
NLI zero-shot models that share code). ``REGISTRY`` in this file is the
single place the CLI/runner looks up candidates by name.
"""

from __future__ import annotations

from .base import Candidate, CandidateUnavailable
from .gliclass import GliClassCandidate
from .kev import KevCandidate
from .nli_zero_shot import make_erlangshen_nli, make_mdeberta_xnli
from .qwen3guard import Qwen3GuardCandidate
from .rule import RuleCandidate
from .setfit_bgem3 import SetFitBgeM3Candidate


def build_registry() -> dict[str, Candidate]:
    """A fresh instance per candidate name every time this is called —
    candidates hold loaded-model state, so the runner should not share one
    across unrelated invocations."""
    return {
        "rule": RuleCandidate(),
        "gliclass-multilang": GliClassCandidate(),
        "mdeberta-xnli-control": make_mdeberta_xnli(),
        "erlangshen-nli": make_erlangshen_nli(),
        "setfit-bge-m3": SetFitBgeM3Candidate(),
        "qwen3guard-0.6b": Qwen3GuardCandidate(),
        "kev-0.8b": KevCandidate(),
    }


__all__ = ["Candidate", "CandidateUnavailable", "build_registry"]
