"""One canonical way to turn an EvalItem into plain text, shared by every
text-only candidate (SetFit, the NLI zero-shot models, GLiClass). Keeping
this in one place means `train_data/tool_risk_train.jsonl`'s hand-written
`text` fields and what the adapters feed a model at eval time are built
the same way.
"""

from __future__ import annotations

from .types import EvalItem


def render_item_text(item: EvalItem) -> str:
    if item.point == "tool_risk":
        text = f"工具调用：{item.input.get('tool')}，参数：{item.input.get('args')}"
        if item.context:
            text += f"；上下文：{item.context}"
        return text
    return item.input
