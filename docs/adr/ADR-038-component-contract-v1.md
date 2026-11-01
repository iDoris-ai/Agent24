# ADR-038：外部组件契约 v1

- 状态：**草案**（2026-11-01，待 jason 评审）
- 依据：[可进化性评估最终版](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md) 建议第 2 条
- 相关：[ADR-K1-01 模块工具契约](ADR-K1-01-module-tool-contract.md)、[ADR-K1-03 启用/授权/信任/吊销](ADR-K1-03-enable-authorization-trust-revocation.md)、CORE-2（能力令牌与提权）、`agent24-os-proto` 握手

## 1. 背景

Agent24 的外部组件（进程外、独立发版的程序）现在各接各的：

| 组件 | 传输 | 版本锁 | 契约版本协商 | 能力声明 |
|---|---|---|---|---|
| iDoris | HTTP（OpenAI 兼容 + Admin） | `agent24-idoris/idoris.lock.json`：tag、commit、各平台 sha256、团队 ID | **有**：`contract_major` / `min_contract_minor`，`check_contract` 拒绝不兼容 | 锁里有 `capabilities`（L1–L6，带来源） |
| Hyphae | 子进程 CLI + JSON envelope | `agent24-comm/hyphae.lock.json`：source_sha、构建配方、各平台 sha256 | **无**：靠 Hyphae 文档约定 | 无 |
| AgentEar | attach socket，JSON-RPC（`os-proto` initialize） | Agent24 不锁，AgentEar 自己装 | **有**：`protocol_versions` 区间协商，`manifest_digest` 校验 | 清单 + `Offer` 授权 |
| Knowledge（WeKnora） | HTTP | 无锁（外部部署） | **有**：`/system/capabilities` 的 `protocol_versions` | `supported_operations`、`limits` |
| Documents | 桌面 main 进程拉起 `agent24-documents` CLI | 同仓库构建，不需要锁 | 无 | 无 |

后果：
- 每接一个新组件，下载、校验、握手、错误处理都要重新发明一遍。
- 下载器是借来的。`agent24-components::fetch_verified` 克隆 iDoris 的锁，再改写它的字段，借用 iDoris 的下载器；缓存目录还必须放在 `idoris` 目录下。Hyphae 的安装就是走的这条路。
- 换成另一个品牌的组件，要重写适配层；对端版本不兼容时，有的组件能在握手时拒绝，有的要到调用出错才发现。

已有的好做法要保留，并推广到所有组件：
- iDoris 的「锁 + 契约版本 + 能力」三件套；
- `os-proto` 的版本区间协商和清单摘要；
- Knowledge 的能力文档。

## 2. 决策

### D1 范围：只管进程外、独立发版的外部组件

**覆盖**：iDoris、Hyphae、AgentEar、Knowledge，以及今后新接的外部程序。Documents 现在是同仓库构建的，只要求它满足 D4–D6；等它独立发版时，再按 D2 补锁。

**不覆盖**：
- 进程内 trait（`ModelProvider`、`Tool`、memory 各 trait）；
- 客户端 ↔ daemon 的 v1 REST/WS（已经有 `protocol_compatibility`）；
- 内部存储替换和数据迁移格式（另立 ADR）；
- MCP（外部标准，照它自己的规范来）；
- 组件内部的业务语义，比如 iDoris 的路由策略、Hyphae 的群聊语义。

### D2 统一锁格式 `ComponentLock`（schema 2）

每个需要 Agent24 下载的组件，在自己的 crate 里放一份 `<id>.lock.json`，结构统一：

```json
{
  "schema": 2,
  "id": "hyphae",
  "version": "v0.27.2",
  "source": { "repo": "https://github.com/iDoris-ai/Hyphae", "commit": "076a5205…" },
  "contract": { "name": "hyphae-cli", "major": 1, "min_minor": 0, "tested": "1.0.0" },
  "release_url_base": "https://github.com/iDoris-ai/Hyphae/releases/download",
  "assets": {
    "darwin-arm64": { "name": "…", "sha256": "…", "files": ["hyphae"] }
  },
  "signing": { "team_id": "HYAP444FN9" },
  "reproducible": { "toolchain": "go1.26.4", "recipe": "…" },
  "capabilities": [ { "id": "dm", "source": "…" } ]
}
```

- `id`、`version`、`contract`、`assets` 是必填字段。`signing`、`reproducible`、`capabilities` 可以不填，但填了就要校验。
- 平台键统一用 `<os>-<arch>`（`darwin-arm64`、`linux-x64` …）。某个平台没有资产，就不写这个键，不写 `null`。
- `agent24-components` 拥有 `ComponentLock` 类型和通用下载器：GitHub 主机白名单、大小上限、sha256 校验、取消、进度、每个组件一把安装锁。iDoris 和 Hyphae 都改用它，`fetch_verified` 不再借用 iDoris 的锁。
- 缓存和安装目录统一放在 `<托管根>/components/<id>/<version>/`。
- 现在的 schema 1 锁文件要能读：先在加载时转换成 schema 2，迁移完成后再删掉转换。

### D3 契约版本：语义化的 major/minor，握手时检查

- 每个组件都要声明自己对外的契约名和版本 `major.minor`。规则：major 变化表示不兼容；minor 只能做加法；未知字段必须忽略。
- Agent24 在锁里写下可以接受的 `major` 和 `min_minor`。版本检查在**第一次交互时**进行，按传输方式不同（见 D4），不能等到业务调用出错才发现。
- 版本不兼容时，统一报错 `component_contract_mismatch`，带上 `{id, expected, actual}`。组件状态置为 `incompatible`，不尝试降级运行。
- 在版本检查之前，组件不得接到任何业务调用。

### D4 一份契约，三种传输绑定

不强求统一成一种传输方式。三种绑定实现同一组语义：版本、能力、健康、错误、取消。

| 绑定 | 版本与能力从哪里来 | 现有对应 |
|---|---|---|
| **HTTP** | `GET /capabilities`（或组件已有的同类端点），返回 `contract_version` 和能力列表 | iDoris `/capabilities`、Knowledge `/system/capabilities` |
| **CLI** | `<bin> contract --json`：stdout 输出一个 JSON 对象，退出码 0 | Hyphae（新增这个子命令） |
| **JSON-RPC attach** | `initialize` 的 `protocol_versions` 区间加 `capabilities` | `os-proto` initialize（AgentEar），不改 |

CLI 绑定额外规定：
- 每次调用的 stdout 只能输出一个 JSON envelope：`{ "ok": true, "data": … }` 或 `{ "ok": false, "error": { "code", "message" } }`；
- 退出码只区分成功（0）、业务错误（1）、契约或用法错误（2）；
- stdin 用来传秘密，秘密不得出现在 argv 或环境变量里。

### D5 生命周期与健康

所有组件对 Agent24 都呈现同一组状态：

`absent → installing → installed → starting → ready ⇄ degraded → stopped`，另有终态 `incompatible`、`failed`。

- 常驻型组件（iDoris serve、Hyphae daemon）由 Agent24 托管，必须提供健康探测：HTTP 绑定用 `/health`，CLI 绑定用 `<bin> health --json`，attach 绑定用连接存活。
- 按次调用型组件（CLI 单次命令）没有 `starting/ready` 状态，只用 `installed`、`incompatible`。
- 停止时先发 SIGTERM，等宽限期（锁里可以配置，默认 15 秒），之后才允许 SIGKILL。组件要保证在宽限期内落盘。
- 状态统一通过 `GET /api/v1/components` 对客户端暴露。能力中心、通信页、本地模型页都从这里读状态，不再各写一套。

### D6 错误、超时与取消

- 错误统一成 `{ code, message, retryable }` 三元组。`code` 用组件自己的命名空间，例如 `hyphae.relay_unreachable`。Agent24 只认 D3/D5 定义的通用码，其余原样透传给上层。
- 每次调用都有超时，由 Agent24 设定。超时后：
  - HTTP 绑定断开连接；
  - CLI 绑定先发 SIGTERM，宽限期后发 SIGKILL；
  - JSON-RPC 绑定发 `$/cancel`。
- 超时后结果算「未知」，处理方式沿用 ADR-K1-01 §2.5：会改变状态的调用不自动重试。

### D7 授权

- 组件拿到的权限，只能来自 CORE-2 按组件发放的能力令牌，范围 = 锁里的 `capabilities` 与宿主授予的交集。组件不得持有 legacy 全权 token。
- 组件暴露给 agent 的工具，走 ADR-K1-01 的模块工具契约。本 ADR 不重复规定。
- 安装、升级、首次启用，都要用户确认（沿用现有的系统确认框）。只读的状态查询不需要确认。

### D8 升级与替换

- 升级同一个组件：改锁，跑契约兼容用例（§3），通过后才能合入。major 升级必须同时提交适配层改动和迁移说明。
- 换成另一个品牌、同类的组件：只要它实现了同一个契约名和 major，Agent24 就不改代码，只改锁。做不到时，在 `agent24-components` 外面写一个适配器，把它包成同一个契约名。不允许改调用方。

## 3. 验收与反例测试

每个组件迁到 v1 时，必须带上这些测试，并证明修复前失败：

1. 锁里的 sha256 和下载的资产不一致 → 拒绝安装，不留下半安装的目录。
2. 对端报的 major 不一致，或 minor 低于 `min_minor` → 状态变成 `incompatible`，不发任何业务调用。
3. 对端返回未知字段 → 照常工作。
4. 调用超时 → 按 D6 取消，状态变化可观测，不自动重试会改变状态的调用。
5. 常驻组件健康探测失败 → `degraded`，恢复后回到 `ready`。
6. 秘密只经 stdin 传递：在 argv、环境变量、日志里都找不到。
7. 用一个 mock 的第二实现跑通全部用例，证明替换边界成立。

## 4. 迁移顺序

| 片 | 内容 | 涉及仓库 |
|---|---|---|
| C1 | 在 `agent24-components` 里加 `ComponentLock` 和通用下载器；`fetch_verified` 改用它；iDoris 锁升到 schema 2（行为不变） | Agent24 |
| C2 | Hyphae 新增 `contract --json`、`health --json`，统一 envelope 和退出码；Hyphae 锁补 `contract` 字段 | Hyphae + Agent24 |
| C3 | 新增 `GET /api/v1/components`，统一状态机；三个页面改为读这个接口 | Agent24 |
| C4 | AgentEar：只在 `/api/v1/components` 里登记，握手不变；补 §3 用例 | Agent24 |
| C5 | Knowledge：把 `/system/capabilities` 映射到 D3/D5 | Agent24 |
| C6 | Documents：等它独立发版时再补 D2 的锁 | — |

C1 最先做，因为它不改任何外部行为，而且能马上去掉 `fetch_verified` 借用 iDoris 锁的问题。C2 要和 Hyphae 一起做，可以和 Hyphae relay 上线后的接线安排在一起。

## 5. 后果

- **好处**：
  - 接一个新组件，只需写一份锁、选一种绑定，测试照 §3 抄；
  - 换品牌只改锁或者加适配器；
  - 三个页面共用一套状态，不再各说各话。
- **代价**：
  - Hyphae 要新增两个子命令；
  - iDoris 和 Hyphae 的锁格式迁移要过渡一段时间；
  - `GET /api/v1/components` 是新的公开接口，要进 v1 契约测试。
- **风险**：契约名和 major 的划分如果太粗，「换品牌」仍然要写适配器。v1 接受这一点，不追求零适配。

## 6. 待决问题（请 jason 拍板）

1. 托管根目录统一成 `~/.agent24/components/<id>/<version>/`，还是保留现在各组件自己的目录、只统一结构？
2. 宽限期默认 15 秒（和 iDoris 一致），可以吗？
3. 第三方组件的签名要求：v1 只要求 sha256 加可选的团队 ID，签名验证放到 ADR-K1-03 / ME-6。可以吗？
4. Hyphae 的 `contract --json` 和 `health --json` 由 Hyphae 会话实现，排在 relay 上线之后，还是并行做？
