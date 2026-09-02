# TODO — 从前到后的开发清单（P4 门快照）

> 本文件是 **面向人的、从前到后的导航清单**。
> 唯一状态源仍是 [`TASKS.md`](./TASKS.md)（loop 工作依据 + 完整设计论证），本文件只做**排序 + 门的可视化**。
> 快照时间：2026-09-03 ｜ 已合并至 #148（v0.3.0 之后：M-E 领域 OS、M-D 记忆重做、F4 Nostr 收官、FU-32 入站活性）。
> ⚠️ 快照行天生会过期，别把它当状态源——状态源是 [`TASKS.md`](./TASKS.md)。
>
> **部署约定（2026-07-30 用户确认）**：测试阶段跑在**用户本机**；mac mini 只承接**真正部署**。
> 故 F5（Mac mini 7×24 泡测）不在测试阶段执行，推迟到真部署。
>
> **F5 状态（2026-09-03）**：代码侧阻塞已清空（FU-32 已合并 #147）。起跑前还需三件环境准备，
> 见 [`../SOAK-F5.md`](../SOAK-F5.md)：指定 `A24_SPEAKER_BIN`（上游已把二进制改名为 `hyphae`）、
> 建 Nostr identity、daemon 加 `--notify=false --auto-reply=false`。

---

## 一眼看全局

```
P0 无人值守正确性 ────────────────── ✅ 全清
P1 能对话的渠道 ─────────────────── ✅ 代码全清（F5 物理泡测 → 真部署时做）
P2 任何人可基于它做 agent ────────── ✅ 全清
P3 更自主/更聪明 ─────────────────── ✅ 主线清（3 个 deferred 小片）
────────────────────────────── 🚪 P4 门（需用户拍板才越）
P4 生态 / 分发（M4/M5） ─────────── ◐ Marketplace ✅；4 个门后项待拍板
```

**结论**：门前所有可编码任务已完成。"从前到后开发"的下一步 = **跨 P4 门**，而门后 4 项按用户约定需拍板。

---

## P0 — 无人值守正确性 ✅

| 任务 | 说明 | 状态 |
|---|---|---|
| G1+H3 消息线程 + durable resume + payload 完整性哈希 + 陈旧性重校验 | 24/7 下凌晨审批不再 fail-closed 死掉，重启复原而非全 abort | ✅ #70 / #72 |

## P1 — 能对话的渠道 ✅（代码）

| 任务 | 说明 | 状态 |
|---|---|---|
| H11 Fake 渠道 harness | FakeWeChat / FakeNostr，渠道审批可自动测 | ✅ #81 |
| F3 微信渠道 | WeChat iLink 官方 Bot API：入站→run，审批经微信 y/n + durable resume | ✅ #78 / #79（实机需扫码） |
| F4 Nostr 渠道 | agent-speaker subprocess + 信封/意图协议 + npub 白名单 + gated 入站；strfry 真 NIP-33 relay 定论 | ✅ #85–#95 |
| F1b 托盘常驻 | 菜单栏 daemon 实时状态 + 启停/重启，4s 轮询 | ✅ #97 |
| **F5 7×24 泡测** | Mac mini 连跑 7 天日程照跑无人工干预 | ⏸ **物理任务 → 真部署时执行**（非测试阶段） |
| R3 headless 加密解锁 | 上游 agent-speaker 挂账 | ⏸ 上游、非阻塞 |

## P2 — 任何人可基于它做 agent ✅

| 任务 | 说明 | 状态 |
|---|---|---|
| E1 agent24-mcp | rmcp client，MCP 工具注入 registry（自动继承审批门） | ✅ #54 |
| E4 agent24d 作 MCP server | `agent24 mcp` 暴露 `agent24_run` + 只读自省，守门留在 daemon | ✅ #73 |
| E3 module.schema 落地 | 安装门禁校验（H10 基座） | ✅ #74 |
| H10 安装同意摘要 | 严格校验 + 默认 disabled pending consent + 安装绝不写 override | ✅ #75 |
| E2 node-host | ~~5 模块 JSON-RPC 接入~~ | descoped（MCP 取代） |
| E5 PGL manifest | RESERVED 占位，无消费者 | ⏸ deferred |

## P3 — 更自主 / 更聪明 ✅（主线）

| 任务 | 说明 | 状态 |
|---|---|---|
| H1 risk_class 加法迁移 | `read/write_local/exec/external`，`requires_approval` 派生 | ✅ #63 |
| H2 用户本地风险 override | glob 规则，模块/persona 不得写入 | ✅ #61 |
| H4 external 定向常驻授权 | `tool→确切目标`，停用宽泛 approve_for_session | ✅ #62 |
| H5 self-wake（时间型） | `self_wake{prompt, after_secs\|at}` 建一次性 schedule | ✅ #76 |
| H8 plan mode + propose_plan | 只读门禁 explore→提交计划→人批→退出只读 | ✅ #82 |
| H9 只读 explorer subagent | 独立上下文、只读工具集、禁递归 | ✅ #66 |
| H12 provider 错误人话翻译 | 额度/权限/模型不存在类落可读文案 | ✅ #65 |
| G2 对外/不可撤回判据 | 被 H1(External) + H4 覆盖 | ✅（无需独立 PR） |
| G3 CLI wrapper 授权策略 | 包二进制不 vendor 源码，写进 SPEC-001 §10 | ✅ #77 |
| — self-wake 事件型 `wake_on_event` | 需事件订阅机制，落不进 At schedule | ⏸ deferred（可做，小片） |
| H7 工具并发三分法 | 收益不确定、代价高 | ⏸ deferred（可能永不做） |
| G1/H3 PR-3 遗留 | append 幂等契约测 + best-effort 契约测 | ⏸ 非阻塞小尾 |

---

## 🚪 P4 门 —— 需用户拍板才越

> 门后 4 项均为**产品级生态 / 对外分发**，用户约定：越门需拍板，不擅自跨。
> Marketplace（M4 已开工部分）不在门后，已完成。

### M4 已完成（门前，Marketplace）✅
- ✅ 模块发现服务（npm scope 扫描 + 信任分层）`module-discovery.ts` + `GET /api/modules/discover`（#90/#91）
- ✅ Desktop 市场浏览 UI（搜索 + 信任级 chip + 已装/未装过滤，debounced）（#94/#97）
- ✅ 一键 install（默认停用，走 H10 consent 再启用）
- ✅ 信任分层显示（官方/社区/第三方，anti-spoof 从包名推导）+ 权限确认

### 🔒 门后待拍板（front-to-back 建议序）

| # | 任务 | 性质 | 对外风险 | 备注 |
|---|---|---|---|---|
| 1 | **iDoris 主 AI 接入**（替换 placeholder） | 内部（换模型/AI 后端） | 低（不对外发数据） | 最内向、最基础，惠及全局；建议优先 |
| 2 | **M5 模块签名 + AirAccount 信任根** | 安全基础设施 | 中（信任根、发布链路） | sigstore 签名 + "只信任 X 签发" |
| 3 | **Nostr 分发 skill 更新** | 对外分发 | 高（发布到 relay） | skill 更新经 Nostr 广播 |
| 4 | **跨用户 skill 共享**（匿名 trajectory） | 对外 + 隐私 | 高（共享用户轨迹） | 用户自愿，匿名化是硬约束 |

### M5 其余（更靠后，同属门后）
- [ ] 跨设备记忆同步
- [ ] 个人 ↔ 组织 ↔ 公共 三级 agent 网络
- [ ] Tauri 2.0 mobile 端（ADR-018）
- [ ] monorepo 拆分时机评估（底线：M5 前不拆）

---

## 下一步（本轮）

门前已无可编码任务。要"从前到后继续开发"就必须跨 P4 门 —— 而门后 4 项按约定需你拍板。
待你指定先做哪一项（建议序见上表：iDoris 主 AI 接入 → 模块签名 → Nostr 分发 → 跨用户共享），
我即从该项开工：先出设计/拆片，再按 loop 规矩逐 PR 推进。

> 非门后、可不拍板即做的零星项（如需填空档）：`wake_on_event`、G1/H3 PR-3 幂等尾测。
