"""D0-8: generic "SetFit 式" head — a frozen sentence-embedding backbone
plus a small classifier head, trained on the same ``train_data/*.jsonl``
every other few-shot candidate uses.

This is **not** the ``setfit`` library's contrastive-pair trainer (that is
what ``setfit_bgem3.py`` keeps using, unchanged, as the bge-m3 baseline).
It is a plainer and more portable substitute: encode each few-shot
sentence with ``sentence-transformers``, fit ``sklearn.LogisticRegression``
on the embeddings. The two are not guaranteed to produce identical numbers
for the same backbone — that trade-off is deliberate: it is the only
training method that works unmodified across every D0-8 family (Qwen3
causal base `.encode()` support varies, e5 needs a query/passage prefix,
bge-m3 already has its own adapter) without hand-tuning a contrastive
trainer per family. PLAN-DECIDE's "同系列尺寸连贯" requirement is about
comparing sizes *within* a family consistently, which this gives — cross
-family comparison against ``setfit-bge-m3`` should be read as "a different
head recipe on a different backbone", not an apples-to-apples ablation of
head type alone.

One instance of :class:`EmbeddingHeadCandidate` per (family, size); see the
factory functions at the bottom for the concrete D0-8 roster (Qwen3
-Embedding 0.6B/4B/8B, multilingual-e5 small/base/large/large-instruct,
paraphrase-multilingual-MiniLM-L12-v2, KaLM-embedding-v2.5 — reference
only, see DECISION-MODELS.md §10 license table).
"""

from __future__ import annotations

import time

from ..overlap import check_overlap, render_overlap_report
from ..render import render_item_text
from ..train_data import load_train_rows
from ..types import LABELS, EvalItem, Point, Prediction
from .base import Candidate, CandidateUnavailable


class EmbeddingHeadCandidate(Candidate):
    applicable_points: tuple[Point, ...] = ("retain_intent", "recall_gate", "tool_risk")

    def __init__(
        self,
        name: str,
        model_id: str,
        revision: str,
        query_prefix: str = "",
        trust_remote_code: bool = False,
    ) -> None:
        super().__init__()
        self.name = name
        self.model_id = model_id
        self.revision = revision
        # e.g. multilingual-e5's required "query: " prefix (model card: use
        # "query: "/"passage: " for every input, even single-text
        # classification — we treat every item as a "query"). Empty for
        # backbones with no such convention (Qwen3-Embedding, bge-m3,
        # MiniLM).
        self.query_prefix = query_prefix
        self.trust_remote_code = trust_remote_code

    def _encode(self, texts: list[str]):
        prefixed = [self.query_prefix + t for t in texts]
        return self._model.encode(prefixed, normalize_embeddings=True, show_progress_bar=False)

    def load(self) -> None:
        try:
            from sentence_transformers import SentenceTransformer
            from sklearn.linear_model import LogisticRegression
        except ImportError as exc:
            raise CandidateUnavailable(f"sentence-transformers/scikit-learn not importable: {exc}") from exc

        try:
            self._model = SentenceTransformer(
                self.model_id, revision=self.revision, trust_remote_code=self.trust_remote_code
            )
        except Exception as exc:  # noqa: BLE001
            raise CandidateUnavailable(f"failed to load {self.model_id}@{self.revision}: {exc}") from exc

        overlap_hits = check_overlap(points=list(self.applicable_points))
        self.overlap_warning = render_overlap_report(overlap_hits, threshold=0.7).strip() if overlap_hits else None

        self._heads: dict[Point, object] = {}
        self.train_counts: dict[Point, int] = {}
        for point in self.applicable_points:
            try:
                rows = load_train_rows(point)
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
                X = self._encode(texts)
                clf = LogisticRegression(max_iter=2000)
                clf.fit(X, ys)
            except Exception as exc:  # noqa: BLE001
                raise CandidateUnavailable(f"failed to train head for {point} on {self.model_id}: {exc}") from exc
            self._heads[point] = (clf, labels)

        try:
            import huggingface_hub

            info = huggingface_hub.model_info(self.model_id, revision=self.revision)
            self.resolved_sha = info.sha
        except Exception:  # noqa: BLE001
            self.resolved_sha = self.revision

    def predict(self, item: EvalItem) -> Prediction:
        point = item.point
        clf, labels = self._heads[point]
        text = render_item_text(item)

        t0 = time.perf_counter()
        try:
            X = self._encode([text])
            probs = clf.predict_proba(X)[0]
        except Exception as exc:  # noqa: BLE001
            latency_ms = (time.perf_counter() - t0) * 1000
            return Prediction(label=None, p=None, latency_ms=latency_ms, backend=self.name, raw=f"inference failed: {exc}")
        latency_ms = (time.perf_counter() - t0) * 1000

        probs = [float(p) for p in probs]
        # clf.classes_ is the label-id order sklearn fit on, not necessarily
        # 0..n-1 in LABELS order if a class was absent from train_data — map
        # back through label2id to be safe.
        label2id = {lab: i for i, lab in enumerate(labels)}
        id2label = {i: lab for lab, i in label2id.items()}
        best_pos = max(range(len(probs)), key=lambda i: probs[i])
        best_label = id2label[int(clf.classes_[best_pos])]
        return Prediction(label=best_label, p=probs[best_pos], latency_ms=latency_ms, backend=self.name, raw=probs)


# ---------------------------------------------------------------------------
# D0-8 factories. Revisions are pinned HF commits, resolved 2026-10-07 (see
# DECISION-MODELS.md §10 for license status per model).
# ---------------------------------------------------------------------------


def make_qwen3_embedding(size: str) -> EmbeddingHeadCandidate:
    revisions = {
        "0.6b": ("Qwen/Qwen3-Embedding-0.6B", "97b0c614be4d77ee51c0cef4e5f07c00f9eb65b3"),
        "4b": ("Qwen/Qwen3-Embedding-4B", "5cf2132abc99cad020ac570b19d031efec650f2b"),
        "8b": ("Qwen/Qwen3-Embedding-8B", "1d8ad4ca9b3dd8059ad90a75d4983776a23d44af"),
    }
    model_id, revision = revisions[size]
    return EmbeddingHeadCandidate(name=f"qwen3-embed-{size}", model_id=model_id, revision=revision)


def make_e5(size: str) -> EmbeddingHeadCandidate:
    revisions = {
        "small": ("intfloat/multilingual-e5-small", "614241f622f53c4eeff9890bdc4f31cfecc418b3"),
        "base": ("intfloat/multilingual-e5-base", "d128750597153bb5987e10b1c3493a34e5a4502a"),
        "large": ("intfloat/multilingual-e5-large", "3d7cfbdacd47fdda877c5cd8a79fbcc4f2a574f3"),
        "large-instruct": ("intfloat/multilingual-e5-large-instruct", "274baa43b0e13e37fafa6428dbc7938e62e5c439"),
    }
    model_id, revision = revisions[size]
    return EmbeddingHeadCandidate(name=f"e5-{size}", model_id=model_id, revision=revision, query_prefix="query: ")


def make_minilm_multilingual() -> EmbeddingHeadCandidate:
    return EmbeddingHeadCandidate(
        name="minilm-multilingual",
        model_id="sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2",
        revision="e8f8c211226b894fcb81acc59f3b34ba3efd5f42",
    )


def make_kalm_embedding_v25() -> EmbeddingHeadCandidate:
    # F. 参考对照 — license 未按 §10 核实到可商用结论，仅测不推荐。
    return EmbeddingHeadCandidate(
        name="kalm-embed-v2.5-REFERENCE",
        model_id="KaLM-Embedding/KaLM-embedding-multilingual-mini-instruct-v2.5",
        revision="52c687bbe81a62a223c924698b787ec05c9a978a",
        trust_remote_code=True,
    )
