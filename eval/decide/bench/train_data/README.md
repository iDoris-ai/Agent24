# SetFit(bge-m3) 训练集（仅供 D0-5 少样本训练，不是评测集）

与 `../../../eval/decide/*.jsonl`（三份评测集）**严格分开**：

- 这里的例句全部手写，没有一条取自 `eval/decide/` 的 101/100/64 条评测样本（不同措辞、不同人名/过敏原/工具名，也没有逐条改写）。
- 评测脚本（`decide_bench/adapters/setfit_bgem3.py`）只用这三份文件 fit 一个 SetFit 分类头，评测永远跑在 `eval/decide/*.jsonl` 上；两边互不混用。
- `tool_risk_train.jsonl` 的 `text` 字段用的是跟推理时同一套渲染规则（`decide_bench.render.render_tool_risk_text`），不是原始 `{tool,args}` 字典，因为 SetFit 只能喂文本。
- 每类 8–15 条，覆盖主要场景但不追求穷尽——SetFit 本来就是少样本方法，训练集大小本身也是报告要交代的一项。
