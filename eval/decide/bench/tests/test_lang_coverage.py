"""三语覆盖守门：评测集与训练集的 lang 字段、每语言最低条数、每语言各标签都有样本。

阈值来自 eval/decide/README.md（retain_intent / recall_gate 每语 ≥60，tool_risk 每语 ≥40；
训练集每类每语 ≥8）。删掉 lang 字段或把某语言删到阈值以下，这里就会变红。
"""

import collections
import importlib.util
import json
from pathlib import Path

from decide_bench.data import EVAL_DIR
from decide_bench.types import LABELS

LANGS = {"zh", "en", "th", "mixed"}
EVAL_MIN = {"retain_intent": 60, "recall_gate": 60, "tool_risk": 40}
TRAIN_MIN_PER_LABEL = 8


def _rows(path: Path) -> list[dict]:
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def test_eval_sets_have_lang_and_per_language_minimums():
    for point, minimum in EVAL_MIN.items():
        rows = _rows(EVAL_DIR / f"{point}.jsonl")
        assert all(r.get("lang") in LANGS for r in rows), point
        for lang in ("zh", "en", "th"):
            sub = [r for r in rows if r["lang"] == lang]
            assert len(sub) >= minimum, (point, lang, len(sub))
            missing = set(LABELS[point]) - {r["expected"] for r in sub}
            assert not missing, (point, lang, missing)


def test_train_sets_have_lang_and_eight_per_label_per_language():
    for point in EVAL_MIN:
        rows = _rows(EVAL_DIR / "bench" / "train_data" / f"{point}_train.jsonl")
        assert all(r.get("lang") in LANGS for r in rows), point
        c = collections.Counter((r["lang"], r["label"]) for r in rows)
        for lang in ("en", "th"):
            for label in LABELS[point]:
                assert c[(lang, label)] >= TRAIN_MIN_PER_LABEL, (point, lang, label, c[(lang, label)])


def test_lang_stats_script_runs():
    spec = importlib.util.spec_from_file_location("lang_stats", EVAL_DIR / "lang_stats.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    out = mod.stats()
    assert "| th |" in out and "| en |" in out
    assert "`ri-" in mod.th_review()
