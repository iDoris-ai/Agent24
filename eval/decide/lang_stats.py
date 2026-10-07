"""统计三份评测集与训练集的 语言 × 标签 分布，输出 Markdown 表格（README 里的分布表由它生成）。

    python3 eval/decide/lang_stats.py            # 打印分布表
    python3 eval/decide/lang_stats.py --th-review  # 打印泰文复核清单（TH_REVIEW.md 正文）

只依赖标准库。
"""

from __future__ import annotations

import collections
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
LANGS = ["zh", "en", "th", "mixed"]
SETS = {
    "retain_intent": ["remember", "forget", "ask_memory", "correct", "preference", "none"],
    "recall_gate": ["need", "no_need"],
    "tool_risk": ["low", "medium", "high", "must_review"],
}


def load(path: Path) -> list[dict]:
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def table(rows: list[dict], labels: list[str], label_key: str) -> str:
    c = collections.Counter((r["lang"], r[label_key]) for r in rows)
    langs = [l for l in LANGS if any(r["lang"] == l for r in rows)]
    out = ["| lang | " + " | ".join(labels) + " | 合计 |", "|---|" + "---|" * (len(labels) + 1)]
    for lang in langs:
        cells = [c[(lang, lab)] for lab in labels]
        out.append(f"| {lang} | " + " | ".join(map(str, cells)) + f" | {sum(cells)} |")
    tot = [sum(c[(l, lab)] for l in langs) for lab in labels]
    out.append("| 合计 | " + " | ".join(map(str, tot)) + f" | {sum(tot)} |")
    return "\n".join(out)


def cost_table(rows: list[dict]) -> str:
    c = collections.Counter((r["lang"], r["cost_level"]) for r in rows)
    langs = [l for l in LANGS if any(r["lang"] == l for r in rows)]
    out = ["| lang | high | medium | low |", "|---|---|---|---|"]
    for lang in langs:
        out.append(f"| {lang} | {c[(lang, 'high')]} | {c[(lang, 'medium')]} | {c[(lang, 'low')]} |")
    return "\n".join(out)


def stats() -> str:
    parts = []
    for name, labels in SETS.items():
        rows = load(HERE / f"{name}.jsonl")
        parts.append(f"**{name}（{len(rows)}）** — expected\n\n{table(rows, labels, 'expected')}\n\ncost_level\n\n{cost_table(rows)}\n")
    parts.append("**训练集 `bench/train_data/`** — label\n")
    for name, labels in SETS.items():
        rows = load(HERE / "bench" / "train_data" / f"{name}_train.jsonl")
        parts.append(f"{name}_train（{len(rows)}）\n\n{table(rows, labels, 'label')}\n")
    return "\n".join(parts)


def th_review() -> str:
    out = []
    for name in SETS:
        rows = [r for r in load(HERE / f"{name}.jsonl") if r["lang"] == "th" or (r["lang"] == "mixed" and any("฀" <= ch <= "๿" for ch in json.dumps(r, ensure_ascii=False)))]
        out.append(f"## {name}（{len(rows)} 条）\n")
        for r in rows:
            inp = r["input"] if isinstance(r["input"], str) else f"`{r['input']['tool']}` {r['input']['args']}"
            ctx = f" ／ context: {r['context']}" if r.get("context") else ""
            out.append(f"- [ ] `{r['id']}` [{r['expected']}] {inp}{ctx}")
        out.append("")
    for name in SETS:
        rows = [r for r in load(HERE / "bench" / "train_data" / f"{name}_train.jsonl") if r["lang"] == "th"]
        out.append(f"## 训练集 {name}_train（{len(rows)} 条，按行号）\n")
        all_rows = load(HERE / "bench" / "train_data" / f"{name}_train.jsonl")
        for i, r in enumerate(all_rows, start=1):
            if r["lang"] == "th":
                out.append(f"- [ ] L{i} [{r['label']}] {r['text']}")
        out.append("")
    return "\n".join(out)


if __name__ == "__main__":
    print(th_review() if "--th-review" in sys.argv else stats())
