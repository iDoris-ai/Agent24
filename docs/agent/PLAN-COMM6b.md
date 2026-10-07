# COMM-6b：桌面通信配置操作

状态：规划；基线 `ab/comm`（COMM-6a 已合入 #678/#679）。每片按独立 task PR 拆分，非锁文件改动不超过 300 行；超出即继续拆片。COMM-6b 只补身份、联系人、relay 与导入操作，不含 COMM-7 的消息、历史、outbox 或重试。

## 基线与依赖门

- #678（`331666a`）提供通过 Electron IPC backend proxy 的类型化 COMM 客户端：读 identity/contact/relay/daemon，启停 daemon，解锁。客户端拒绝畸形 envelope/关键字段，不直连网络。
- #679（`5e0b850`）提供通信概览、显式 daemon 控制和解锁；刷新保留旧值并提示可能过期，异步响应按序号丢弃。导入、身份创建/默认切换、联系人操作、relay 编辑/探测均未包含。
- Rust `agent24-comm` 已有 `POST /identity`、`POST /identity/default`、`POST /contact`、`PUT /relay`、`POST /relay/probe`、`POST /import`；这些写路径的服务端验证、KeystoreWriteLock、daemon 重启及导入 staging 逻辑属于既有边界。桌面端不得直接读 HOME、spawn Hyphae 或绕过 typed IPC 客户端。
- Hyphae #132 已合入其 `main`（当前 main `6c613b6` 包含该改动），但尚未发布到 Agent24 lock。它让 kind 30078 profile 必须有唯一 `c=profile` 与 `d=agent-profile`，消息必须通过唯一标签组合分类；profile 查询/解析也使用该分类。凡会发布或解析 profile 事件的功能，先等 Hyphae 发版，再完成 DEP-C8 升级 `hyphae.lock.json`，确认锁定源码/构建产物已包含 #132 后才能开工。单纯 relay/contact UI 不触碰 profile 事件，可立即开工。
- 高风险定义沿用 `AGENTS.md`：安全/权限/授权、数据写入语义、迁移、对外协议/wire 格式。高风险 task PR 必须等 PR-Daemon APPROVE 后才合入 `ab/comm`。

## Task 拆分

### COMM-6b.1：联系人新增表单

- **开工：可立即开工。** 仅使用现有 `POST /comm/contact`，不读写 profile 事件。
- **高风险：是（keystore 数据写入）。** 必须等 PR-Daemon APPROVE。
- **范围：** 为概览页增加联系人新增表单，字段 nickname、npub、可选 role；补齐 typed API 方法及参数/响应校验。现有联系人继续只读展示。冻结 REST 没有联系人更新/删除路由，因此本片不提供已有联系人改名或删除，也不扩展 Rust 协议。
- **可证伪验收：** 先写组件/API 测试并在 base 上确认失败；有效输入恰好调用一次 `POST /api/v1/comm/contact`，请求体与输入一致；空 nickname、非法 npub、pending 时不能提交；服务端拒绝后保留列表并显示错误、不显示成功提示；成功后重新读取列表且新联系人出现；双击只产生一次写请求。页面无直接 fetch、HOME 访问或进程启动。

### COMM-6b.2：relay 编辑与手动探测

- **开工：可立即开工。** relay 配置/探测路径不发布或解析 profile 事件。
- **高风险：是（relay 配置写入；探测会访问 relay 并写入手动探测状态）。** 必须等 PR-Daemon APPROVE。
- **范围：** relay 整体替换编辑器及显式探测按钮；调用现有 `PUT /comm/relay` 与 `POST /comm/relay/probe` typed API。保持 1..=8 条、仅 ws/wss URL 的服务端契约；展示 source/configured 和最近一次 probe，不能把握手成功写成补收完成或消息送达。无自动探测、无周期探测。
- **可证伪验收：** 测试先于实现并在 base 失败；无效/0 条/超过 8 条/非 ws(s) 地址不会发请求；有效保存恰好一次 PUT 且一次请求整体替换完整列表；探测只在按钮点击时恰好调用一次，默认不带 URL 时按现有接口契约请求；失败保留旧配置并标记错误；成功刷新配置和 probe 状态。保存进行中禁止重复写入；probe 成功不会改变 catch-up 文案。

### COMM-6b.3：身份创建与默认身份切换

- **开工：等 Hyphae 发版 + DEP-C8 升级 `hyphae.lock.json`。** 创建身份可能发布 kind 30078 profile 事件，身份信息读取/验证也可能解析 profile；锁升级及其验证完成前不得实现或联调这些路径。
- **高风险：是（身份/密钥材料写入及默认身份切换）。** 必须等 PR-Daemon APPROVE。
- **范围：** 发版门通过后，为现有身份列表增加创建、默认身份切换；使用 `POST /comm/identity` 与 `POST /comm/identity/default` typed wrappers。密码生成/钥匙串策略仍由 Rust 服务端负责；renderer 不接触口令、keystore 或 profile wire 格式。切换默认身份时提示 daemon 可能重启，并按服务端状态刷新。
- **可证伪验收：** 在发版门通过后先加测试，并证明 base 失败；创建请求对精确昵称只发送一次，成功后新身份出现且恰好一个 `default=true`；默认切换只发送一次目标 nickname，刷新后默认标记唯一且落在目标身份；空昵称、pending、拒绝响应不显示成功；并发旧刷新不能覆盖操作后的新身份状态；测试断言 UI 不生成/保存/记录密码。PR 记录 Hyphae release、DEP-C8 commit 与 lock 中 source SHA。

### COMM-6b.4：旧 HOME 导入向导与导入门

- **开工：等 Hyphae 发版 + DEP-C8 升级 `hyphae.lock.json`。** 导入预检会通过 `identity list` 等路径读取身份/profile，必须确认解析规则来自含 #132 的发布版本。
- **高风险：是（跨目录数据复制/持久化与导入授权）。** 必须等 PR-Daemon APPROVE。
- **范围：** UI 先运行 `dry_run` 并展示身份、联系人、outbox、数据库文件计数；明确要求用户确认已停止使用源目录，再单独确认正式导入。正式导入按现有后端 contract 提交 from/confirm/password；不自行复制文件、不启 daemon。密码只在提交时存在于受控输入态，默认不记住；完成或失败均清空。沿用 COMM-6a pending、错误及异步序列保护。
- **可证伪验收：** 测试先加并在 base 失败；未完成 dry-run、计数异常、未勾选停止使用确认或未确认正式导入时，正式 `POST /import` 调用次数为 0；dry-run 恰好一次且 `dry_run=true`；确认后正式请求恰好一次且 `confirm=true`；取消/失败不声称导入成功，密码在成功与失败后都清空；成功后重新载入 identity/contact/relay；用坏源路径、目标非空、源占用、明文 keystore、错误口令的服务端错误夹具验证 UI 不把它们显示为成功。PR 记录 Hyphae release、DEP-C8 commit、锁 SHA，并确认导入前后源目录未被 UI 修改。

## 顺序与完成定义

```text
可立即开工：6b.1 联系人新增 ─┐
可立即开工：6b.2 relay 编辑/探测 ├─ 可并行；逐片 task PR ≤300 行
Hyphae release + DEP-C8 门：6b.3 身份操作 ─┤
Hyphae release + DEP-C8 门：6b.4 导入向导 ─┘
```

每个 task 都先写可证伪测试，在原 base 上确认失败，再实现并记录红/绿结果；每个 PR base 为 `ab/comm`。页面写操作共享 pending 防重入，刷新与写入继续采用序列号防旧响应覆盖。COMM-6b 全部合入后，回填 `tasks.md` 状态/证据；COMM-7（收件历史/outbox/重试与双仓联调）仍单独排期。
