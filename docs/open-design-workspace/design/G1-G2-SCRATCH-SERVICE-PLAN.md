# G1/G2：Scratch Workspace 服务与 Host Lease 计划

> 状态：Design frozen / implementation not started
>
> 基线：`main@220d5b8`；store implementation `16d674e`
>
> 覆盖：A24-OD-01 G1/G2。本文消费 ADR-002/005，不修改其公开契约。

## 1. 完成边界

现有 `Store::create_workspace` 只把调用方提供的 root、identity 和 generation 写入
SQLite。它已完成 registry 的 DB 语义，但**不是**安全的文件系统 root allocator，
也没有产生可交给 Run 的 `WorkspaceHandle`。

G1/G2 只交付未接线的 scratch service core：

- 由可信 daemon state dir 分配和 pinned reopen root；
- 以持久化 allocation journal 对账 FS 与 DB；
- host lease acquire/renew/release core；
- Creative capability 已绑定 workspace 的 exact cwd resolve core；
- pure auth guard、错误优先级、恢复与对抗测试。

本波禁止 Run/Session migration 或 binding、run lease、真实 cleanup、sweeper、router
注册、`AppState`/main 初始化、Electron/ACP 产品接线。通过本波也不代表 Creative
route 已开放。

## 2. 服务所有权

新增 `agent24-workspace` crate，拥有目录 authority、pinned root 和 service；store
不反向依赖它。公开 create spec 不接受 ID、路径、identity 或 generation。

```rust
pub struct WorkspaceService { /* store, roots, clock, ids */ }
pub struct ScratchCreateSpec { /* provenance, owner, ttl */ }
pub struct HostLeaseOwner { /* trusted daemon + host generations */ }

impl WorkspaceService {
    pub async fn create_scratch(...) -> ServiceResult<Workspace>;
    pub async fn acquire_host_lease(...) -> ServiceResult<HostMount>;
    pub async fn renew_host_lease(...) -> ServiceResult<HostMount>;
    pub async fn release_host_lease(...) -> ServiceResult<()>;
    pub async fn resolve_exact(...) -> ServiceResult<WorkspaceId>;
}
```

`PinnedWorkspaceRoot` 的字段全部私有，不实现 serde，不提供 `from_path`。它只是
crate 内 root authority，不得被表述为已经绑定 Run 的 handle。store 只增加窄化的
internal root snapshot 和 checked admission API；普通 protocol `Workspace` 不暴露
canonical root 或完整 DB row。

业务拒绝使用 committed outcome，而不是会回滚事务的 `Err`：

```rust
enum Admission<T> {
    Granted(T),
    Denied(WorkspaceAdmissionDenial),
}
```

这样 lazy expiry 可先提交 state、revision 和 audit，再映射为 `409`。

## 3. Root allocation 与 identity

固定布局：`<state>/workspace-roots/<workspace-id>.<generation>/`。state dir 只来自
可信配置；名字由服务生成；非 UTF-8 locator fail closed，不做 lossy conversion。

服务持有 managed parent handle。创建和 reopen 都相对该 handle 完成；不得用
`canonicalize -> check -> ambient open`，不得接管已存在目录，也不得因 path 相同就
接受被替换的对象。

- Unix identity：从同一打开 handle 读取 `dev + ino`，均以 `u64` little-endian 保存；
- root/parent symlink、类型或权限不符都拒绝；managed parent 使用 `0700`；
- reopen 后同时核对 path locator、generation 和 persisted identity；
- root 缺失或替换只返回 unavailable，不自动修复或重新登记。

Windows schema 需要真实 64-bit volume serial + 128-bit `FILE_ID_INFO.FileId`。当前
`cap-std 3.4.5` metadata 只有更弱字段，禁止补零、path hash 或降低全局
`unsafe_code=forbid` 冒充实现。必须先交付来自**同一 directory HANDLE**、拒绝
junction/reparse 的 safe platform primitive；否则 Windows G1 明确保持 unavailable。

## 4. FS 与 DB crash consistency

追加 internal `workspace_allocations` journal，阶段为
`reserved -> materialized -> committed`，失败对象进入 `retained`。它记录生成的
workspace/root generation、relative name、parent/root identity 和脱敏失败原因，
不对外公开。

创建顺序：

1. DB 提交 reserved intent；
2. 相对 pinned parent mkdir/open，捕获 handle identity；
3. DB 提交 materialized identity；
4. 单事务复验 intent/identity，写 workspace、audit 与 committed；
5. commit 后重验 handle/path 对应关系，才返回公开 Workspace。

任一不确定或失败点都 fail closed 并保留对象；本波不执行补偿删除。commit 结果不确定
时，只能按 exact workspace ID + generation + identity 对账。DB/FS 冲突不得猜测或
扫描后接管。真正 quarantine/delete 属于 G3。

## 5. Host lease core

heartbeat 建议 30 秒；每次 host lease 固定 90 秒。时间、daemon generation 和
`host_instance_id` 都由可信服务/auth claims 提供，JSON body 不得自选。

- acquire：active scratch + pinned identity；同 current owner 的有效 lease 幂等返回，
  但不隐式续期；旧 daemon lease 在原期限内仍作 cleanup drain 保护；
- renew：校验 exact lease/kind/workspace/root generation/owner/daemon generation；
  过期 lease 不复活；workspace 到期先提交 expiry 再拒绝；identity 失败不延长；
- release：先校验 owner/generation/kind，再做幂等；只释放该行，不删 root/workspace；
- restart：旧 generation 不恢复 mount authority，也不能续期；其保护最多保留原剩余
  90 秒，不能因 restart 重新延长。

每次 admission 在 `BEGIN IMMEDIATE` 内重读、lazy expiry、校验、audit 和 commit。
时钟倒退时 fail closed；宁可保留 lease/root，也不能伪造释放时间。

## 6. Exact cwd resolve

Resolve 只检查 capability claims 已绑定的那个 workspace，不做反向路径搜索：

1. 校验 active `creative_runtime`、Principal resource、无 Origin；
2. 读取 claims 中的 workspace，要求 active scratch 和 current host lease；
3. cwd 必须 absolute、存在、无 NUL/`..`/device ambiguity；
4. canonical locator 必须**精确等于** root，不授权 descendant；
5. 再用 pinned identity 证明仍是同一对象；
6. 事务内重验 state/generation/TTL，响应前重验 capability；
7. 只返回 `workspace_id`，不返回 root、lease 或其他 workspace 是否存在。

Resolve 不创建 workspace、run lease、session scope，也不 renew host lease。sidecar
generation 和 durable entitlement 由后续 G5/G10 接线提供，因此本 core 暂不注册路由。

## 7. Auth 与错误优先级

任何存在的 Origin header 都拒绝，包括空值。host API 只接受 current
`product_host`；legacy bearer、Creative、renderer proxy 都拒绝。顺序固定为：

1. transport/body bound；
2. bearer/revocation/generation：`401`；
3. Origin/audience/action/resource：`403`，不查询目标；
4. 已授权字段错误：`400`；
5. 合法 ID 不存在：`404`；
6. owner/lease/root conflict：`409 workspace_binding_conflict`；
7. 已提交 lazy expiry/released：对应 `409`；cleanup failure 为 `503`；
8. pinned identity/permission/clock：`503`；
9. DB/corrupt row：脱敏 `500`。

日志与 audit 不记录 cwd、root 或 token，只记录 opaque IDs、状态和固定 reason。

## 8. 实现门禁与切片

严格顺序：journal/internal snapshot -> Unix/Windows pinned primitive -> root service ->
host admission/lease -> exact resolve -> aggregate SOL review。

实现继续按单一 invariant 拆分，每 PR changed lines `<=200`：crate skeleton、journal
migration/decoder/transactions、CREATE transaction helper、Unix primitive/tests、Windows
platform gate、service state machine/crash tests、root snapshot/admission、host acquire/renew/
release/recovery、resolve normalization/admission、pure auth guard、aggregate regression。

必测：collision 不接管、parent/root swap、delete/recreate、每个 crash point、commit
uncertainty/cancellation、lease exact-expiry/WAL race/clock rollback、restart old-generation
drain、relative/descendant/cross-root/symlink/junction resolve、audience x Origin x action。
Windows native test 是 Windows gate；macOS cross-compile 不能替代。

Legacy root、历史非终态 Run/approval migration、serial run lease 与 tool handle 注入全部
留到 G4，且必须先解决 ADR-002 中“多个历史非终态 Run”与 serial 唯一 lease 的冲突。
