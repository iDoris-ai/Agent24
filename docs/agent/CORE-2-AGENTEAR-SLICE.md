# CORE-2 纵切：AgentEar 在桌面 capabilities 模式下配对（S1–S4）

> 日期：2026-10-28。协调者：Opus。依据：`docs/agent/CORE-2-ENROLL-DESIGN.md`（冻结稿，§10 优先于 §9）。本文只**裁剪顺序与范围**，不改冻结稿的安全语义；冲突时以冻结稿为准，并在 PR 里指出。
> 起因：桌面拉起的 daemon 运行在 capabilities 子进程模式，`daemon.json` 没有 bearer。AgentEar 代跑 `agent24 os attach add <manifest> --json`（A3 §3.6）时，CLI 报 `host authority unavailable`，配对失败（#1347）。jason 10-28 要求立即修复，不做临时放宽。

## 1. 目标链路

```
AgentEar ─exec→ agent24 os attach add --json
  CLI 读 daemon.json：auth_mode=capabilities 且有 enroll_endpoint
  CLI ─UDS→ enroll.sock：hello → challenge → 身份校验（SecCode）→ enrolled{token}（日常级 FirstPartyShell）
  CLI ─HTTP→ POST /api/v1/attached（Bearer token）→ 403 presence_required{operation,…}
  CLI ─UDS（同一连接）→ elevate{method,path,body_sha256,summary}
       daemon 生成挑战 → 在场证明提供者 = 桌面主进程（ProductHost 长轮询）
       桌面弹原生确认框「AgentEar 请求与 Agent24 配对 —— 允许 / 拒绝」
  daemon → elevated{grant}（60 s，一次性）
  CLI ─HTTP→ POST /api/v1/attached（Bearer token + X-Agent24-Elevation: grant）→ 201
```

## 2. 本纵切范围与推迟项

| 冻结稿片 | 本纵切做 | 推迟（登记到 TODO，不宣称已完成） |
|---|---|---|
| 2c | macOS 身份：`proc_pidpath` + SecCode requirement + identifier 白名单；UDS 握手；pid/启动时间前后复核 | Linux 签名清单、FD 登记权、配对码退路（未签名的开发构建在 capabilities 模式下登记会失败，legacy 不受影响） |
| 2b | `Audience::FirstPartyShell`、令牌签发/校验/进程存活/UDS 续期；D4 提升授权存储与原子消费；§10 R1 模块委托记录 | ProductHost 有限 TTL 与父管道续期（Q4）；开发者令牌（Q6、R3） |
| 2d | 在场证明提供者接口 + **桌面原生确认通道**（D7 第二通道） | Swift LocalAuthentication helper（Touch ID）、headless 提供者 |
| 2f | CLI 登记客户端 + `presence_required` 自动申请提升并重试 | TUI、桌面自身改走登记、LaunchAgent 服务安装、backendProxy 收窄的其余部分 |

capabilities 仍非默认模式（D1）。legacy 行为一字不改。

## 3. 冻结接口

### 3.1 登记 UDS 协议 v1（`<data_dir>/enroll.sock`，一行 JSON 一帧，≤16 KiB）

| 方向 | 帧 | 说明 |
|---|---|---|
| C→S | `{"v":1,"op":"hello","generation":"<daemon.json generation>"}` | generation 不符 → `error{code:"generation_mismatch"}` 并关闭 |
| S→C | `{"v":1,"op":"challenge","conn_id":"<hex16>","nonce":"<hex32>"}` | 客户端不需要回签；nonce 绑定本连接，用于日志与提升挑战关联 |
| S→C | `{"v":1,"op":"enrolled","token":"<opaque>","expires_in":3600,"tier":"daily","identity":{"kind":"macos_seccode","identifier":"agent24"}}` | 身份通过；token 只出现这一次 |
| S→C | `{"v":1,"op":"error","code":"identity_rejected"\|"generation_mismatch"\|"unsupported_version"\|"bad_frame"\|"timeout","message":"…"}` | 随后关闭 |
| C→S | `{"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":"<hex64>","summary":"<≤200 字符，仅供参考>"}` | 只能在已 enrolled 的连接上发 |
| S→C | `{"v":1,"op":"elevation_pending","challenge_id":"<hex16>"}` | 立即回；之后等提供者 |
| S→C | `{"v":1,"op":"elevated","challenge_id":"…","grant":"<opaque>","expires_in":60}` | 一次性 |
| S→C | `{"v":1,"op":"error","code":"presence_denied"\|"presence_unavailable"\|"presence_timeout"\|"not_elevated_operation"\|"unknown_route",…}` | 连接保持，可再发 |
| C→S / S→C | `{"v":1,"op":"renew"}` / `enrolled{…}`（新 token，旧 token 原子作废） | 只在原连接上（R4） |

- 握手各阶段 5 s；等待在场证明阶段 60 s（D4 挑战期限）。
- 连接断开：不撤销已签令牌（冻结稿 D5 第 5 条）；令牌仍受进程存活与 TTL 约束。
- daemon 侧 `summary` 由 daemon **自己**根据 method/path/body 生成后交给提供者展示；客户端的 summary 只记日志（D4 第 1 条：客户端自报摘要不能替代 daemon 摘要）。

### 3.2 HTTP

- 令牌：`Authorization: Bearer <token>`。FirstPartyShell 校验：未过期、generation 一致、绑定进程 `(pid,start_time)` 仍存活且一致、Operation 在日常级目录内或带有效提升授权。
- 提升：`X-Agent24-Elevation: <grant>`。daemon 校验 grant 绑定的 token 摘要、method、路由模板、实际 path、`sha256(body)` 全部一致，**原子消费**后才进处理器。
- 无授权访问提升级操作：`403 {"error":{"code":"presence_required","message":…,"operation":"ModuleAdmin"}}`，无副作用。
- 授权已消费/过期/不匹配：`403 {"error":{"code":"elevation_invalid",…}}`。

### 3.3 在场证明提供者（daemon 内部 trait，S2 定义、S3 实现）

```rust
pub struct PresenceRequest { pub challenge_id: String, pub operation: Operation, pub method: String,
    pub path: String, pub summary: String /* daemon 生成 */, pub requester: RequesterIdentity, pub expires_at: UnixSeconds }
pub enum PresenceOutcome { Allowed, Denied, Unavailable, TimedOut }
#[async_trait] pub trait PresenceProvider: Send + Sync {
    async fn confirm(&self, req: PresenceRequest, cancel: CancellationToken) -> PresenceOutcome;
}
```

无提供者（桌面未连接）→ `Unavailable` → `presence_unavailable`。

### 3.4 桌面确认通道（ProductHost 专用路由，S3）

| 方法 | 路径 | Operation | 允许的 Audience |
|---|---|---|---|
| GET | `/api/v1/presence/challenges/next?wait=25` | PresenceRead | 仅 ProductHost |
| POST | `/api/v1/presence/challenges/{id}` body `{"decision":"allow"\|"deny"}` | PresenceDecide | 仅 ProductHost |

- 长轮询：有挑战即返回 `200 {challenge_id, operation, summary, requester:{identifier,pid}, expires_at}`，否则 `204`。
- 桌面**主进程**轮询并用 `dialog.showMessageBox` 弹原生框（三语，默认英文，走 `mt()`），按钮「Deny」为默认与取消键。renderer 无法触达：backendProxy 白名单显式拒绝 `/presence/`。
- 与冻结稿 D7 的差异：子进程模式下桌面是 ProductHost 父进程，不经登记；以 ProductHost 令牌鉴权的主进程通道等价于「已登记桌面主进程的通道」。2f 桌面改走登记后改为 FirstPartyShell + 桌面身份。
- 同一挑战只接受一次决定；过期挑战决定返回 `410`。

### 3.5 路由等级

沿用冻结稿 §3。`POST /api/v1/attached` = ModuleAdmin = 提升。新增两条 presence 路由进 2a 的路由覆盖门（T1）。

## 4. 分片与派工

| 片 | 内容 | 依赖 | 派给 |
|---|---|---|---|
| S1 2c-mac | 新 crate `agent24-os-proc`（默认 deny unsafe，仅窄封装允许）：`proc_pidpath`、进程启动时间、SecCode-by-pid requirement `anchor apple generic and certificate leaf[subject.OU] = "HYAP444FN9"` + identifier ∈ {`ai.auraai.agent24`,`agent24`,`agent24d`}，且 SecCode 路径与 `proc_pidpath` 一致；非 macOS 返回 `Unsupported`。`enroll_endpoint.rs`：§3.1 的 hello/challenge/身份/前后复核，签发经 `TokenIssuer` trait（S2 实现，S1 用测试桩） | — | mini Codex |
| S2 2b-min | FirstPartyShell 令牌（claims：pid、start_time、身份摘要、tier、generation；TTL 3600；内存只存摘要）；HTTP 校验接入 route_policy；D4 提升授权（挑战 60 s、授权 60 s、原子消费、并发最多一成功）；`PresenceProvider` trait 与 elevate/renew 处理；§10 R1 模块委托记录 | S1 接口 | mini Codex |
| S3 2d-desktop | §3.4 两条路由与挑战队列；桌面主进程长轮询 + 原生确认框（三语）；backendProxy 拒绝 `/presence/` | S2 trait | 本机 Sonnet |
| S4 2f-CLI | CLI：capabilities 模式发现 enroll_endpoint → 登记 → 带令牌请求；收到 `presence_required` 时在同一连接 elevate，stderr 打印等待提示，拿到 grant 后重试一次；`os attach add --json` 输出形状不变（A3 §3.6）；错误码映射 `presence_denied`/`presence_unavailable` 进 `--json` 错误体 | §3.1/§3.2 | 本机 Sonnet |
| S5 集成验收 | 端到端：capabilities 子进程模式 daemon + 假桌面提供者（自动 allow/deny）+ 真 CLI `os attach add --json` → 201 / `presence_denied`；签名构建真机：桌面 + AgentEar 配对成功 | S1–S4 | 协调者 |

PR 全部进 `ab/core`，S1/S2 先合；S3/S4 以冻结接口并行开发、按 S2 落地后 rebase。纵切完成后 ab/core → main 发版，并在 #1347 通知 AgentEar（AgentEar 侧无需改动：它照旧代跑 CLI，只是会看到桌面弹确认框）。

## 5. 测试门（每片先 red 再 green）

- S1：非白名单 identifier、未签名二进制、路径与 SecCode 不一致、pid 复核时启动时间变化、generation 不符、超长帧、超时 → 全部拒绝且不发令牌；uid 不符拒绝。
- S2：日常操作放行；提升操作无授权 → `presence_required` 且处理器未执行；授权换 path/body/method → `elevation_invalid`；并发两次消费只成功一次；进程退出后令牌立即失效；续期后旧令牌失效；HTTP 上无续期；R1 四负例 + 一正例。
- S3：非 ProductHost 访问 presence 路由 403；重复决定只认第一次；过期 410；桌面拒绝/超时 → `presence_denied`/`presence_timeout`；renderer 经 backendProxy 访问 `/presence/` 被拒。
- S4：legacy 模式行为不变（快照）；capabilities 模式 attach add 成功输出与 A3 §3.6 快照同形；denied/unavailable 的 `--json` 错误体。
