# `_a24/assistant/chat`：附着模块调用 Agent24 助手

2026-11-21，jason 拍板，进 0.7.0。

## 为什么

AgentEar 双击说话，附着 Agent24 后现在走 `_a24/model/complete`（ME4-S2）：只是裸调一次模型，AgentEar 自己拼 system 提示，不带 Agent24 的记忆、知识库和会话。jason 的要求是：**连接了 Agent24 时，双击就等于跟 Agent24 助手说话**。助手用默认模型（经 iDoris），带上默认赋予的上下文（个人记忆、已开启的知识库），给出回答。没有连接时 AgentEar 保持原样。

## 接口

新增内核回调方法 `_a24/assistant/chat`，和 `_a24/model/complete` 并列，由同一套附着/挂载路径注册。

### 授权

- 新能力 `Capability::Assistant`，清单里写作 `assistant`（`kernel_capabilities: [assistant]`）。
- 和 `models` 一样：模块申请且被授予才可用；没授予时方法照样注册，调用返回 `forbidden`。
- `initialize` 结果的 `offer.provides` 里出现 `_a24/assistant/` 前缀，AgentEar 据此判断宿主支持助手接口。

### 入参

```json
{
  "text": "今天下午有什么安排？",
  "lang": "zh-CN",
  "conversation_id": "ae-3f9c2a",
  "request_id": "req_01J..."
}
```

| 字段 | 规则 |
|---|---|
| `text` | 必填，用户这一句话（语音转写结果），1..=4000 字节，去掉首尾空白后不能为空 |
| `lang` | 可选，`zh-CN` / `en-US` / `th-TH`，默认 `zh-CN`。只用来提示回答语言 |
| `conversation_id` | 必填，AgentEar 自己维护的会话标识，`[A-Za-z0-9_-]{1,64}`。同一个 id 的多轮对话落在同一个 Agent24 会话里 |
| `request_id` | 可选，≤128 字节，原样回传，用于日志关联 |

未知字段拒绝（`invalid`）。

### 行为

1. **会话**：`(模块名, conversation_id)` 映射到一个 Agent24 会话；第一次用到时新建，标题「语音对话」（随 lang 三语）。映射存在 daemon 的库里，重启后仍能接上。这个会话出现在桌面聊天页的会话列表里，用户可以回看。
2. **上下文**：和桌面聊天页同一套主流程：
   - 知识库开启且 `default` 绑定在 daemon 里处于激活状态 → 走 agent loop（与 `POST /api/v1/runs` 相同的创建路径，带 `knowledge_binding_id = "default"`），等它结束。
   - 否则 → 走 `POST /api/v1/chat` 带 `session_id` 的路径（个人记忆召回 + 写回）。
   - 两条路径都复用现有代码，不复制逻辑；只在消息最前面加一条短的 system 提示：「回答会被朗读：用 {lang} 回答，口语、简短（一般不超过 3 句），不要 Markdown、列表或代码。」
3. **模型**：不指定模型，走默认路由（经 iDoris），与聊天页一致。
4. **超时**：整个调用最多 90 秒（知识库路径包含检索）。超时取消这次 run 并返回 `timeout`。
5. **需要审批**：agent loop 停在等待用户审批时，不等，直接返回 `awaiting_approval`，run 留在桌面端由用户处理。
6. **并发**：每个模块同时最多 1 个助手调用，多出的返回 `busy`。双击是一问一答，不需要更多。

### 返回

```json
{
  "text": "下午三点有个评审会，之后没有安排。",
  "session_id": "ses_01J...",
  "run_id": "run_01J...",
  "request_id": "req_01J..."
}
```

- 非流式，一次返回完整文本，长度 ≤8000 字节（超过截断，截断处不切坏 UTF-8）。
- `text` 是给 AgentEar 朗读的纯文本；AgentEar 负责 TTS，宿主不会再用 `speak` 重复播报。

### 失败码

沿用 `RpcError` 的封闭集合：

| 情况 | 错误 |
|---|---|
| 没授予 `assistant` | `forbidden` |
| 入参不合法 | `invalid` |
| 已有一个助手调用在进行 | `busy` |
| 模型不可用（iDoris 未就绪、503 等） | `unavailable`，message 带 iDoris 给的 reason |
| 超过 90 秒 | `timeout` |
| 等待用户在桌面审批 | `awaiting_approval`（新增；如果封闭集合不便扩展，用 `unavailable` + `data.reason = "awaiting_approval"`，实现时二选一并写进本节） |
| 会话或记忆存储失败 | `internal`，不带内部路径或 stderr |

## 不做（FU）

- 流式返回、边生成边朗读。
- 一个模块多个并发助手调用。
- 语音里处理审批（只提示去桌面确认）。

## AgentEar 侧（由 AgentEar 仓库实现）

- `provides` 含 `_a24/assistant/` 且清单拿到 `assistant` 授权 → 双击走 `_a24/assistant/chat`，不再拼自己的 system/天气提示。
- 否则保持现状（附着时 `_a24/model/complete`，未附着时本机小模型）。
- 回答文本由 AgentEar 的 TTS 朗读；`awaiting_approval` 时念一句「需要在 Agent24 里确认」。
