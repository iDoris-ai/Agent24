"""One canonical way to turn an EvalItem into plain text, shared by every
text-only candidate (SetFit, the NLI zero-shot models, GLiClass). Keeping
this in one place means `train_data/tool_risk_train.jsonl`'s hand-written
`text` fields and what the adapters feed a model at eval time are built
the same way.

D0-8 三语 fix: `tool_risk`'s `tool`/`args`/`context` values are themselves
localized per the item's own `lang` (e.g. an `en`-tagged item's `args` is
an English query string, a `th`-tagged one's is Thai) now that the
trilingual eval set (ab/decide-01, merged into ab/decide #726) carries an
explicit `lang` per row. The old rendering wrapped every language's
content in a fixed Chinese template ("工具调用：…，参数：…") — reviewer
feedback on #726 flagged that this glues a Chinese prefix onto an
English/Thai sentence, which is wrong both for a human reading the text
and for `decide_bench.lang.infer_lang`'s CJK/Thai character check. Fixed
here with one language-**neutral** template (plain `tool: … args: …`
labels) rather than three hand-translated ones — translating "工具调用"/
"参数"/"上下文" into `en`/`th` would need upkeep every time a label
changes, and the field names themselves carry enough meaning without
translation.
"""

from __future__ import annotations

from .types import EvalItem


def render_item_text(item: EvalItem) -> str:
    if item.point == "tool_risk":
        text = f"tool: {item.input.get('tool')} args: {item.input.get('args')}"
        if item.context:
            text += f" context: {item.context}"
        return text
    return item.input
