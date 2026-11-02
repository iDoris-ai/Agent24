# OfficeAssistant M1 样本检查与评测工具

OfficeAssistant M1 的合成会议样本（转录 + 金标准）的工具。任务见 [`../../docs/agent/PLAN-OFFICE-ASSISTANT.md`](../../docs/agent/PLAN-OFFICE-ASSISTANT.md) OA-M1-02（样本与金标准）和 OA-M1-03（抽取评测脚本）。样本格式见 [`FORMAT.md`](FORMAT.md)（英文，草案 v0，待 jason 确认）。

## 文件

| 文件 | 作用 |
|---|---|
| `check_sample.py` | 检查一份样本：转录首行必须是 SYNTHETIC 声明；金标准字段取值合法；每条证据的行号存在、引文逐字出现在该行 |
| `test_check_sample.py` | 上面的测试（用代码现场生成最小样本，不依赖真实样本文件） |
| `FORMAT.md` | 样本格式说明 |
| `samples/` | 样本目录（`<id>.transcript.txt` + `<id>.gold.json`）。样本由后续的数据 PR 提供 |

## 运行

只用 Python 3 标准库，无需安装依赖。在仓库根执行：

```bash
python3 -I eval/office-assistant/check_sample.py <sample_id> [样本目录]   # 默认目录：eval/office-assistant/samples
python3 -I eval/office-assistant/test_check_sample.py -v
```

退出码：`0` 无问题，`1` 发现问题，`2` 用法错误。

目前 CI 不运行 Python 测试（`.github/workflows/` 与根 `package.json` 都没有 Python 步骤），这些测试需要本地手动运行。是否接入 CI 待 jason 决定。
