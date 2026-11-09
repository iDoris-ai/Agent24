# 常驻 B 期：桌面端与 LaunchAgent 守护进程的连接（M5 B1–B3）

依据：跟踪 #1708；B2 #1711、B3 #1717；守护进程侧 K-B3 #1715（presence 帧定稿，issuecomment-6732349252）。

`agent24 service install` 之后，守护进程由 LaunchAgent 运行（`<data_dir>/bin/agent24d serve --port 0 --auth-mode capabilities --enroll-endpoint`）。这时桌面端不再自己 spawn 守护进程，也拿不到宿主令牌，改为通过 daemon.json 里的 `enroll_endpoint`（一个 Unix socket）连上去。下面的两条连接都开在这个 socket 上。

## 1. token 连接（B2，`daemon-enroll.ts`）

- 协议 v1：一行 JSON 一帧，每帧 `v:1`，连换行最多 16 KiB。
- 握手：守护进程先发 `challenge`（32 位十六进制 nonce）→ main 回 `hello{generation, nonce}` → 守护进程回 `enrolled{token, http_port}`。`http_port` 必须等于 daemon.json 的端口，否则令牌不用。
- 令牌是 FirstPartyShell 的日常令牌，TTL 1 小时。main 每 5 分钟在同一条连接上 `renew` 一次，比守护进程的 10 分钟空闲断开早。新令牌一到，旧令牌就作废。
- 连接断了就在下一个 tick 重新登记。登记被拒（未签名的构建、版本不符）时，状态是 `error`，详情是 `enrol_refused`：不 spawn，按 5 秒到 5 分钟的指数退避重试，换了守护进程（generation 变化）就立即重试（B3a）。
- 令牌不打印、不记日志。

## 2. presence 连接（B3，`presence-connection.ts`）

宿主令牌时（自己的 child），守护进程通过 HTTP `/api/v1/presence` 让 main 确认，由 `presence-poller.ts` 处理。服务模式下 main 没有宿主令牌，守护进程改为在第二条连接上询问：

| 方向 | 帧 | 说明 |
|---|---|---|
| main → | `hello{role:"presence", generation, nonce}` | 不要令牌 |
| ← daemon | `presence_ready{identity}` | 这条连接上出现 `enrolled` 帧算错误 |
| ← daemon | `presence_challenge{challenge_id, nonce, operation, method, path, summary, requester{identifier,pid}, expires_at}` | 同一时间最多一个 |
| main → | `presence_answer{challenge_id, nonce, decision: allow\|deny}` | 只在 challenge 来的那条连接上回 |
| ← daemon | `presence_answered{challenge_id, accepted}` | 迟到或重复的回答 `accepted:false`，连接不断 |
| ← daemon | `presence_cancel{challenge_id}` | 关掉对应的框，不回答 |
| ← daemon / main → | `presence_ping{nonce}` / `presence_pong{nonce}` | 每 60 秒一次，10 秒内要回 |

- main 收到 challenge 就弹原生框（与 HTTP 轮询共用 `presenceDialogOptions`）。默认按钮和取消都是「拒绝」，框出错也按拒绝回答。
- 过期、被 cancel、连接断开时，关框且不回答。断开时，守护进程把没回答的 challenge 记为 Unavailable。
- 第二个 challenge 到来时前一个还开着，或者 challenge 格式不对：断开连接，退避 1–30 秒后重连。超过 130 秒没收到 ping 也算断。
- 只在登记着服务守护进程时连（`BackendManager.presenceTarget()`）。这期间 HTTP 轮询器闲置。

## 3. 两段式提权（B3，`daemon-enroll.ts` `elevate`、`elevated-request.ts`）

哪些操作要确认，只由守护进程的 `route_policy.rs` 决定。日常令牌访问 Elevated 路由时，守护进程回 `403 presence_required`。main 照 CLI 的 `send_elevating` 处理：

1. 先用日常令牌发请求。
2. 收到 `403 presence_required`，并且当前连接是服务守护进程的登记时，在 token 连接上发 `elevate{method, path, body_sha256, summary}`。守护进程把确认请求转给 presence 连接（见第 2 节），用户同意后回 `elevation_pending{challenge_id}` → `elevated{challenge_id, grant}`。main 最多等 65 秒。
3. 带 `X-Agent24-Elevation: <grant>` 用同一个请求重试一次。grant 有效期 60 秒、只能用一次，绑定方法、路径和请求体，所以重试发的正是哈希过的那份字节。

- elevate 和 renew 在 token 连接上串行执行：守护进程等用户时收到任何输入都会断开；而且 grant 绑定在当前令牌上，所以重试也在同一轮内完成，中间不会插进 renew。
- 用户没有确认时（拒绝、超时、没有 presence 连接），页面收到 403，`code` 用守护进程给的错误码。不会再试第二次。
- 带查询串的请求不提权，守护进程会拒绝这种 grant，所以原样返回 403。
- 只有 main 设置这个头：页面的请求（`BackendProxyRequest`）不带自定义头，grant 也不离开 `sendElevating`。
- 自己的 child（宿主令牌）和 legacy 守护进程没有登记连接，只发一次普通请求，行为和以前一样。
- `ipc/index.ts` 里经 `proxyToBackend` 的请求（通用代理和各个专用 IPC）都走这条路。main.ts 里直接调用 `proxyRequest` 的几处只读请求（runs、schedules、chat）不涉及 Elevated 路由，没有改。

## 4. 已知局限

- 真守护进程上的端到端验证（点确认弹原生框、确认后审批生效），要等 K-B3（含三类操作的摘要）进 main 后再做。在那之前只在假 socket 上测过。
- `summary` 只是兜底文案（`Agent24: <方法> <路径>`）。守护进程认识的操作（K-B3 的 ApprovalDecision、ScheduleManage、Shutdown，以及 Knowledge 的 bindings）由它自己写摘要。
