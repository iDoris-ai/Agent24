#!/usr/bin/env python3
"""D0-8: export a winning embedding backbone to ONNX (int8 dynamic
quantization) and bench onnxruntime's RSS/latency for it — the concrete
first step of the Rust `ort` integration route PLAN-DECIDE's deep-layer
design points at.

This script is deliberately **not** wired into ``decide_bench``'s own
``pyproject.toml`` dependencies: ``optimum[onnxruntime]`` needs
``transformers<4.47``, while this project's base dependency ``gliclass``
needs ``transformers>=5.0`` — a genuine, unresolvable conflict for one
shared lockfile (see the NOTE in ``pyproject.toml``). So this runs in its
own throwaway environment:

    uv run --isolated --with 'optimum[onnxruntime]>=1.20,<2.2' --with 'transformers<4.47' \\
        --with torch --with onnx --with scikit-learn \\
        python scripts/export_onnx.py --model-id intfloat/multilingual-e5-base \\
        --revision d128750597153bb5987e10b1c3493a34e5a4502a --out-dir /tmp/onnx-e5-base

It does three things:
  1. Export the sentence-embedding backbone to ONNX via
     ``optimum.onnxruntime.ORTModelForFeatureExtraction`` + dynamic int8
     quantization (``optimum.onnxruntime.quantization``).
  2. Re-train the exact same LogisticRegression head
     ``adapters/embed_head.py`` trains (reusing ``train_data/*.jsonl``
     through ``decide_bench.train_data``), but feeding it embeddings
     computed from the ONNX session instead of the PyTorch model, so the
     accuracy claim is "this ONNX export reproduces the PyTorch numbers",
     not a separate untested path.
  3. Benchmark onnxruntime inference alone over the three published eval
     sets: peak RSS (periodic sampling, not a single before/after delta —
     DECISION-MODELS.md §9.4 flags the delta approach as unreliable under
     memory pressure), load time, P50/P95 latency. The LogisticRegression
     head itself is a few hundred floats times a tiny matrix — its cost is
     negligible next to the backbone and is not separately reported.

Writes ``<out-dir>/onnx_export_report.json`` with all of the above, in a
shape close enough to a ``CandidateResult`` that ``report.py`` conventions
still make sense when quoting it in DECISION-MODELS.md §10 by hand (this
script does not try to merge into the main bench's results/*.json, since
it does not share a Python environment with it).
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from pathlib import Path

# repo layout: eval/decide/bench/scripts/export_onnx.py -> eval/decide/bench
_BENCH_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(_BENCH_DIR / "src"))


def _peak_rss_sampler(interval_s: float = 0.05):
    """Periodic-sample peak RSS (MB) in a background thread, per the D0-6
    finding (DECISION-MODELS.md §9.4) that a single before/after
    ``getrusage`` delta under memory pressure can be unreliable — here we
    actually poll ``psutil`` instead."""
    import psutil

    proc = psutil.Process()
    state = {"peak_mb": 0.0, "stop": False}

    def _loop():
        while not state["stop"]:
            rss_mb = proc.memory_info().rss / (1024 * 1024)
            if rss_mb > state["peak_mb"]:
                state["peak_mb"] = rss_mb
            time.sleep(interval_s)

    t = threading.Thread(target=_loop, daemon=True)
    t.start()
    return state, t


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model-id", required=True)
    ap.add_argument("--revision", required=True)
    ap.add_argument("--query-prefix", default="", help='e.g. "query: " for multilingual-e5')
    ap.add_argument("--out-dir", required=True)
    ap.add_argument(
        "--sets",
        default="retain_intent,recall_gate,tool_risk",
        help="comma-separated decide_bench.types.Point names to benchmark latency over",
    )
    args = ap.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    from optimum.onnxruntime import ORTModelForFeatureExtraction
    from optimum.onnxruntime.configuration import AutoQuantizationConfig
    from optimum.onnxruntime import ORTQuantizer
    from transformers import AutoTokenizer
    import numpy as np
    import onnxruntime as ort
    from sklearn.linear_model import LogisticRegression

    from decide_bench.data import load_set
    from decide_bench.render import render_item_text
    from decide_bench.train_data import load_train_rows
    from decide_bench.types import LABELS, Point

    fp32_dir = out_dir / "fp32"
    int8_dir = out_dir / "int8"

    state, _thread = _peak_rss_sampler()
    t_load0 = time.perf_counter()

    tokenizer = AutoTokenizer.from_pretrained(args.model_id, revision=args.revision)
    model = ORTModelForFeatureExtraction.from_pretrained(
        args.model_id, revision=args.revision, export=True
    )
    model.save_pretrained(fp32_dir)
    tokenizer.save_pretrained(fp32_dir)

    quantizer = ORTQuantizer.from_pretrained(fp32_dir)
    qconfig = AutoQuantizationConfig.avx512_vnni(is_static=False, per_channel=False)
    quantizer.quantize(save_dir=int8_dir, quantization_config=qconfig)
    tokenizer.save_pretrained(int8_dir)

    onnx_path = next(int8_dir.glob("*.onnx"))
    fp32_size_mb = sum(f.stat().st_size for f in fp32_dir.glob("*") if f.is_file()) / (1024 * 1024)
    int8_size_mb = sum(f.stat().st_size for f in int8_dir.glob("*") if f.is_file()) / (1024 * 1024)

    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    load_latency_s = time.perf_counter() - t_load0

    def encode(texts: list[str]) -> np.ndarray:
        prefixed = [args.query_prefix + t for t in texts]
        enc = tokenizer(prefixed, padding=True, truncation=True, return_tensors="np")
        outputs = sess.run(None, {k: v for k, v in enc.items() if k in {i.name for i in sess.get_inputs()}})
        last_hidden = outputs[0]  # (batch, seq, hidden)
        mask = enc["attention_mask"][:, :, None]
        summed = (last_hidden * mask).sum(axis=1)
        counts = np.clip(mask.sum(axis=1), 1e-9, None)
        mean_pooled = summed / counts
        norm = np.linalg.norm(mean_pooled, axis=1, keepdims=True)
        return mean_pooled / np.clip(norm, 1e-9, None)

    # Re-train the same head this backbone's embed_head.py candidate uses,
    # but on ONNX-computed embeddings, so the accuracy number below is
    # about THIS export, not assumed from the PyTorch run.
    points: list[Point] = [p.strip() for p in args.sets.split(",")]  # type: ignore[assignment]
    heads = {}
    for point in points:
        rows = load_train_rows(point)
        labels = LABELS[point]
        label2id = {lab: i for i, lab in enumerate(labels)}
        X = encode([r["text"] for r in rows])
        y = [label2id[r["label"]] for r in rows]
        clf = LogisticRegression(max_iter=2000)
        clf.fit(X, y)
        heads[point] = (clf, labels)

    results = {}
    for point in points:
        items = load_set(point)
        clf, labels = heads[point]
        label2id = {lab: i for i, lab in enumerate(labels)}
        id2label = {i: lab for lab, i in label2id.items()}
        latencies = []
        correct = 0
        for item in items:
            text = render_item_text(item)
            t0 = time.perf_counter()
            X = encode([text])
            probs = clf.predict_proba(X)[0]
            best_pos = int(np.argmax(probs))
            pred = id2label[int(clf.classes_[best_pos])]
            latencies.append((time.perf_counter() - t0) * 1000)
            if pred == item.expected:
                correct += 1
        latencies.sort()
        p50 = latencies[len(latencies) // 2] if latencies else None
        p95 = latencies[int(len(latencies) * 0.95)] if latencies else None
        results[point] = {
            "n_items": len(items),
            "accuracy": correct / len(items) if items else None,
            "p50_latency_ms": p50,
            "p95_latency_ms": p95,
        }

    state["stop"] = True
    report = {
        "model_id": args.model_id,
        "revision": args.revision,
        "runtime": "onnxruntime (CPUExecutionProvider)",
        "quantization": "dynamic int8 (avx512_vnni config, per_channel=False)",
        "fp32_onnx_size_mb": fp32_size_mb,
        "int8_onnx_size_mb": int8_size_mb,
        "load_and_quantize_latency_s": load_latency_s,
        "peak_rss_mb_periodic_sample": state["peak_mb"],
        "points": results,
        "note": (
            "peak_rss_mb_periodic_sample samples this whole process's RSS every "
            "50ms during export+quantize+inference, per DECISION-MODELS.md §9.4's "
            "caveat about before/after deltas under memory pressure — it is not "
            "isolated to just the onnxruntime session."
        ),
    }
    (out_dir / "onnx_export_report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
