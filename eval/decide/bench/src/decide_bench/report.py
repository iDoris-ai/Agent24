"""Render the JSON results of a bench run into the Markdown summary table
PLAN-DECIDE D0-5 asks for."""

from __future__ import annotations

from typing import Any


def _fmt(x: float | None, digits: int = 3) -> str:
    if x is None:
        return "—"
    return f"{x:.{digits}f}"


def render_markdown(run: dict[str, Any]) -> str:
    lines: list[str] = []
    lines.append(f"# D0-5 候选横评结果 — {run['machine_id']}")
    lines.append("")
    lines.append(f"- 日期：{run['date']}")
    lines.append(f"- 机器：{run['machine_id']}（{run['machine_info'].get('cpu_brand', '?')}, "
                 f"{int(run['machine_info'].get('ram_bytes', 0) or 0) // (1024**3)}GB RAM）")
    lines.append(f"- Python：{run['machine_info'].get('python', '?')}")
    lines.append("")
    lines.append(
        "权重：`cost_weighted_error_rate` 用 high=5 / medium=2 / low=1（`decide_bench.types.COST_WEIGHTS`）。"
        " ECE 只在该候选对该点产出概率时计算，否则记为 `—`。"
    )
    lines.append("")
    lines.append(
        "注：`erlangshen-nli` / `mdeberta-xnli-control` 用同一个中文 hypothesis 模板"
        "（`这句话属于：{}`）跑标准 zero-shot-classification pipeline，没有为每个模型分"
        "别调过模板或 premise/hypothesis 顺序——`erlangshen-nli` 的分数明显偏低，可能部分"
        "来自这一点而不是模型本身在其训练任务上的真实水平，D1 选型前若要用这条路线需要单"
        "独调一遍模板再比。"
    )
    lines.append("")

    unavailable = [c for c in run["candidates"] if not c["available"]]
    available = [c for c in run["candidates"] if c["available"]]

    for point in ["retain_intent", "recall_gate", "tool_risk"]:
        lines.append(f"## {point}")
        lines.append("")
        header = [
            "候选", "n", "弃权", "准确率", "宏F1", "ECE", "代价加权误判率",
            "P50延迟(ms)", "P95延迟(ms)",
        ]
        if point == "retain_intent":
            header += ["误写入率", "记住召回"]
        lines.append("| " + " | ".join(header) + " |")
        lines.append("|" + "---|" * len(header))
        for c in available:
            pr = c["points"].get(point)
            if pr is None:
                continue
            row = [
                c["name"], str(pr["n_items"]), str(pr["n_abstained"]),
                _fmt(pr["accuracy"]), _fmt(pr["macro_f1"]), _fmt(pr["ece"]),
                _fmt(pr["cost_weighted_error_rate"]),
                _fmt(pr["p50_latency_ms"], 1), _fmt(pr["p95_latency_ms"], 1),
            ]
            if point == "retain_intent":
                extra = pr.get("retain_intent_extra") or {}
                row += [_fmt(extra.get("false_write_rate")), _fmt(extra.get("remember_recall"))]
            lines.append("| " + " | ".join(row) + " |")
        lines.append("")

    lines.append("## 资源与下载")
    lines.append("")
    lines.append("| 候选 | model_id | revision | resolved_sha | 加载耗时(s) | 峰值RSS增量(MB) | 下载体积(MB，HF 缓存估算) |")
    lines.append("|---|---|---|---|---|---|---|")
    for c in available:
        lines.append(
            f"| {c['name']} | {c['model_id'] or '—'} | {c['revision'] or '—'} | {c['resolved_sha'] or '—'} | "
            f"{_fmt(c['load_latency_s'], 1)} | {_fmt(c['peak_rss_mb'], 1)} | {_fmt(c['download_size_mb'], 1)} |"
        )
    lines.append("")

    lines.append("## Unavailable")
    lines.append("")
    if not unavailable:
        lines.append("（本次运行全部候选都可用）")
    else:
        for c in unavailable:
            lines.append(f"- **{c['name']}**（{c['model_id'] or '—'}）：{c['unavailable_reason']}")
    lines.append("")
    return "\n".join(lines)
