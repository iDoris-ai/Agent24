"""Candidate adapters for the D0-5 横评.

Each module exposes one ``Candidate`` subclass (or a factory, for the two
NLI zero-shot models that share code). ``REGISTRY`` in this file is the
single place the CLI/runner looks up candidates by name.
"""

from __future__ import annotations

from .base import Candidate, CandidateUnavailable
from .causal_logit import make_qwen3_llm
from .embed_head import (
    make_e5,
    make_kalm_embedding_v25,
    make_minilm_multilingual,
    make_qwen3_embedding,
)
from .gliclass import GliClassCandidate
from .kev import make_kev_0_8b, make_kev_4b
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
        "kev-0.8b": make_kev_0_8b(),
        # --- D0-8: 同系列多尺寸补测 (docs/agent/PLAN-DECIDE.md D0-8) ---
        # A. Qwen3-Embedding + SetFit 式(embedding+LogisticRegression)头
        "qwen3-embed-0.6b": make_qwen3_embedding("0.6b"),
        "qwen3-embed-4b": make_qwen3_embedding("4b"),
        "qwen3-embed-8b": make_qwen3_embedding("8b"),
        # B. multilingual-e5 + 同一头
        "e5-small": make_e5("small"),
        "e5-base": make_e5("base"),
        "e5-large": make_e5("large"),
        "e5-large-instruct": make_e5("large-instruct"),
        # C. 8GB 极小档对照 (paraphrase-multilingual-MiniLM-L12-v2)
        "minilm-multilingual": make_minilm_multilingual(),
        # D. Qwen3 因果 LLM 读 logit（零训练，MLX）
        "qwen3-llm-0.6b": make_qwen3_llm("0.6b"),
        "qwen3-llm-1.7b": make_qwen3_llm("1.7b"),
        "qwen3-llm-4b": make_qwen3_llm("4b"),
        "qwen3-llm-8b": make_qwen3_llm("8b"),
        # E. Kev 同系列次尺寸 (4B)
        "kev-4b": make_kev_4b(),
        # F. 参考对照，许可证未核实到可商用结论，仅测不推荐
        "kalm-embed-v2.5-reference": make_kalm_embedding_v25(),
    }


__all__ = ["Candidate", "CandidateUnavailable", "build_registry"]
