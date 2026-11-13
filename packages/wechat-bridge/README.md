# 微信桥（开发工具）

本包是开发工具，不进产品。它只把微信消息交给 daemon，不再自己决定谁能起 run，也不再在微信里做审批。

行为：

- 只投递 `message_state=FINISH` 且 `message_type=USER` 的消息。生成中的消息和机器人自己的消息不投递。
- 事件 id 是 `wechat:<message_id>`，发到 `POST /api/v1/inbound/events`。`source` 由该路由固定为 `wechat`。服务端受理（`started` / `ignored` / `duplicate` / `already_linked`）之后，iLink 游标才前进。临时故障（网络错误、超时、5xx、429）一直不推进，下一次重投，不因次数放弃。确定性失败才跳过并推进：400/403/413/422、坏时间戳，以及 408 和其余未列出的 4xx 连续超过 20 次（记一条日志后前进）。daemon 按 `wechat:<message_id>` 去重，不会再起一个 run。
- 桥内不再有 uid 名单，也不再 `POST /api/v1/runs`。谁能起 run 由桌面写入 `inbound_allowlist`（放行的微信 uid）。capabilities 模式下种子环境变量 `A24_INBOUND_DEV_ALLOWLIST` 不生效。
- 只投递有文字的消息。纯图片、空文本、只有空白的消息不投递，也不回复。这类消息仍推进游标。`create_time_ms` 缺失、不是有限数、或超出 `Date` 能表示的范围时同样：记一条日志并前进，不发入站，也不回「记录失败」。
- 只有 daemon 返回 `started`（记下并起了 run）时，回一句「已记录」。不回发 run 结果。`ignored`（名单外、过期、限流等）、`duplicate`（同一条消息再次投递，可能是名单外消息的重放）都不回复。
- 「y」「n」是普通文本，不再批准或拒绝。审批只在桌面端做。
- 投递失败回「记录失败，请稍后重试」，不带 daemon 的错误原文。运行中 POST 返回 401 时桥退出，并提示重新签发。返回 404 时桥退出，并提示 daemon 未挂载开发入站路由（需 `A24_DEV_TOOL=1`）。400/403/413/422 记一条失败后前进。408 和其余未列出的 4xx 保持游标并退避，同一条连续超过 20 次后放弃该条并前进。网络错误、超时、5xx、429 一直保持游标并退避，不因次数放弃。POST 之前就能判定的非法时间戳不走这条失败回复，直接前进。

令牌不在 `daemon.json` 里。`agent24 dev bridge-token` 走现有的本机登记和桌面确认，请求 `POST /api/v1/capabilities/bridge`，把 8 小时的组件令牌写到状态目录 `dev/wechat-bridge.token`（目录 0700，文件 0600，JSON 里还有 `capability_id`）。桥只读这个文件里的 `token` 和 `expires_at`。文件缺失、权限不是 0600、或已过期时，桥拒绝启动。

撤销签发它的 shell 不会连带撤销这枚桥令牌。要停掉它，运行 `A24_DEV_TOOL=1 agent24 dev bridge-token --revoke`：读文件里的 `capability_id`，经桌面确认后请求撤销，成功后删除该文件。没有这条命令时，令牌在 8 小时后过期；daemon 重启后 generation 对不上，令牌也会失效。

启动入口默认拒绝运行，只有显式设置 `A24_DEV_TOOL=1` 才继续。daemon 也要带上同一个标记才会挂签发路由和入站路由；capabilities 模式只接受微信桥的组件令牌调用入站。产品启动流程不得设置此标记；它不是抵御同 UID 操作者的安全边界。

```sh
# 桌面以 capabilities 模式启动 daemon，并带上 A24_DEV_TOOL=1。
# 在桌面放行微信 uid（inbound_allowlist）。种子环境变量不写入这份名单。

# 桌面确认后签发。stdout 只有文件路径和过期时间。
A24_DEV_TOOL=1 agent24 dev bridge-token

# 桌面确认后撤销，并删除令牌文件。
A24_DEV_TOOL=1 agent24 dev bridge-token --revoke

# 桥
A24_DEV_TOOL=1 pnpm --filter @agent24/wechat-bridge bridge
```

首次运行打印二维码。端口来自 daemon 状态文件；测试可以用 `A24_BASE_URL` 改端口，不能用环境变量代替令牌文件。桌面打包配置不得引用 `packages/*-bridge`（由桌面打包测试检查）。参见 [ADR-044](../../docs/adr/ADR-044-communication.md)。

## 已知限制

再次签发先把新令牌原子落盘，成功后再尽力撤销替换前仍未过期的旧 id。撤销失败只在 stderr 提示，新文件已经生效。签发或落盘失败时不撤销旧 id，文件里的旧令牌仍然可用。同一用户的并发进程在检查与使用之间替换 `dev/` 父目录，或签发与 `--revoke` 并发时，CLI 和桥无法完全防住：`--revoke` 按路径删除，中间换上来的新文件可能被删掉。Node 没有 `openat`。这是开发工具，需要 `A24_DEV_TOOL=1`。目录这一条本轮不修。连续确定性 4xx 超过 20 次后放弃的那一条消息不再投递；放弃前用户已经收到过一次「记录失败，请稍后重试」，之后不会再投递。daemon 停机（网络、超时、5xx、429）不会因此丢消息。重签之后，正在运行的桥仍持有旧令牌，下一次入站 POST 返回 401 并退出，需要重启桥才会读新文件。新令牌落盘之后如果撤销旧 id 失败，或落盘与撤销之间崩溃，旧 id 不在文件里，`--revoke` 撤不到它，会活到 8 小时过期或 daemon 重启。
