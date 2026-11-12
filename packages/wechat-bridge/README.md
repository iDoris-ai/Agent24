# 微信桥（开发工具）

本包是开发工具，不进产品。它只把微信消息交给 daemon，不再自己决定谁能起 run，也不再在微信里做审批。

行为变化（X11-2g）：

- 只投递 `message_state=FINISH` 且 `message_type=USER` 的消息。生成中的消息和机器人自己的消息不投递。
- 事件 id 是 `wechat:<message_id>`，发到 `POST /api/v1/inbound/events`。`source` 由该路由固定为 `wechat`。
- 桥内不再有 uid 名单，也不再 `POST /api/v1/runs`。名单在 daemon：开发态启动时读 `A24_INBOUND_DEV_ALLOWLIST=wechat:<uid>,...`，幂等写入 `inbound_allowlist`（`note=dev-seed`）。capabilities 模式不读这变量，这条路由也不挂载。
- 只投递有文字的消息。纯图片、空文本、只有空白的消息不投递，也不回复。
- 只有 daemon 返回 `started`（记下并起了 run）时，回一句「已记录」。不回发 run 结果。`ignored`（名单外、过期、限流等）、`duplicate`（同一条消息再次投递，可能是名单外消息的重放）都不回复。
- 「y」「n」是普通文本，不再批准或拒绝。审批只在桌面端做。
- 投递失败回「记录失败，请稍后重试」，不带 daemon 的错误原文。

启动入口默认拒绝运行，只有显式设置 `A24_DEV_TOOL=1` 才允许继续发现 daemon、登录微信。daemon 也要在 legacy 模式、同样带上 `A24_DEV_TOOL=1` 时才会挂这条路由。产品启动流程不得设置此标记；它不是抵御同 UID 操作者的安全边界。

```sh
# daemon：legacy + 开发标记 + 种子名单
A24_DEV_TOOL=1 A24_INBOUND_DEV_ALLOWLIST='wechat:<开发用户 ID>' agent24 daemon start

# 桥
A24_DEV_TOOL=1 pnpm --filter @agent24/wechat-bridge bridge
```

首次运行打印二维码。连接配置使用 `A24_BASE_URL` / `A24_TOKEN` 或开发 daemon 的状态文件。桌面打包配置不得引用 `packages/*-bridge`（由桌面打包测试检查）。参见 [ADR-044](../../docs/adr/ADR-044-communication.md)。
