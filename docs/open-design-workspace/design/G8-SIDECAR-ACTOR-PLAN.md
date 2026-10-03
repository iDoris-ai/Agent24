# G8：Bounded Sidecar Actor 计划

> 状态：Design frozen / implementation not started
>
> 基线：frame reader `abd22a1`；A24-OD-05 manager/protocol/platform owner 已通过
>
> 范围：私有 helper 管理一个测试 target generation；不接 renderer、IPC、路由或真实 OD

## 1. 边界与状态机

一个 helper 进程只拥有一个 target generation。它完成 launch、Owned/Ready、control、
exit 和 stop；重启必须由上层 supervisor 在旧 tree 确认 empty 后创建新 helper，旧 actor
不得重新 Launch，也不能让 pipe EOF 自动继承 authority。

```text
AwaitLaunch -> Launching -> AwaitReady -> Running
           -> GracefulStopping -> ForceStopping -> Draining -> Empty
```

任何 fatal transport/protocol failure 都进入同一清理路径。`Draining` 只能依据 owner
证据转到 `Empty`，或在 deadline/观察失败时转到 `Unconfirmed`。`Unconfirmed` 继续持有
owner、返回失败并禁止新 generation；只有后续 owner-backed force/probe/reap 确认 tree
empty 才能转 `Empty`。parent EOF/cancellation/connection loss 发生在 confirmed Empty 前
时，helper/supervisor 必须保持 blocked cleanup，不能退出、drop owner 或重启。绝不能用
超时冒充 `empty:true`。

## 2. Owner 前置接口

当前 POSIX launch 丢弃三路 stdio；Windows owner 的 `wait()` 会消费 child 并 drop Job。
actor 接线前必须增加窄平台接口，actor 不直接操作 PID/PGID：

```rust
struct OwnedTarget { /* private platform owner */ }
struct TargetPipes { /* stdin/stdout/stderr, each moved once */ }

enum ExitObservation { Running, Exited { code: Option<i32> } }
enum TreeObservation { Present, ConfirmedEmpty, Unconfirmed }

impl OwnedTarget {
    fn launch(...) -> Result<(Self, TargetPipes), OwnerError>;
    fn observe_exit(&mut self) -> Result<ExitObservation, OwnerError>;
    fn request_stop(&mut self, force: bool) -> Result<(), OwnerError>;
    fn observe_tree(&mut self) -> Result<TreeObservation, OwnerError>;
    fn reap_step(&mut self) -> Result<TreeObservation, OwnerError>;
}
```

方法只做短操作/一步轮询，deadline 由 actor 调度。POSIX 保留 generation-pinned process
group；Windows 保留同一 Job handle，soft stop 用 target stdin EOF，force 用 exact Job
kill，不能把 processkit 的 Windows shutdown 当成 SIGTERM。

`TargetPipes.stdin` 的关闭 authority 属于 actor：soft stop 只关闭一次 stdin。关闭后
Windows `request_stop(false)` 是幂等 no-op，`force=true` 才调用 exact
`ProcessGroup::kill_all()`；owner 不得重新取得或复制 stdin。Windows empty probe 必须给
固定 `processkit = 3.3.4` 显式启用 `stats` feature，并读取固定结构的
`stats().active_process_count`；禁止用会分配 PID `Vec` 的 `members()`。

## 3. Allocation-bounded I/O

| 资源 | 固定上限 |
| --- | --- |
| 单次 read | 4096 bytes |
| control frame | 65,536 bytes，含 newline |
| target Ready frame | 16,384 bytes，含 newline |
| decoded/pending control queue | 各 1 |
| writer pending queue | 1 |
| Ready/Exit slot | 各 1 |
| stderr drain buffer | 4096 bytes，不保留 raw ring |
| worker/task 数 | 固定，不按请求或输出 spawn |

禁止无界 `read_line/read_until`、Vec 日志、unbounded channel 和 per-frame task。frame 完成
后移动 Vec，不 clone；严格按 `consumed` 推进；partial EOF 是协议失败。单-slot writer
必须继续 partial write，不能取消半帧后写下一条；writer 阻塞不能阻止 owner force/observe。

Wire frame 有界不等于 JSON heap 有界。必须在 protocol crate 增加 bounded Serde
visitor/seed，在 materialize 前计数并拒绝重复 top-level key、所有平台重复 env key及
Windows 大小写等价 env key；actor 禁止先调用现有 `decode_request`。visitor 成功后才
允许 `RequestSequence::accept`。Launch 约束：argv 最多 128、env
最多 64、单字符串复用 4096 上限；env key 非空且无 `=`/控制字符，Windows 按平台语义
拒绝大小写重复。数量检查必须在 materialize 容器前完成。

stderr 只 drain 并累计固定计数，禁止打印 child raw bytes；其中可能含 token。

## 4. Launch、Ready 与 sequence

1. 首条请求必须是合法 Launch；version=1、ID 非零、executable/cwd absolute；
2. Command 不经 shell，`env_clear()` 后只设置显式 env；
3. 先建立完整 containment 并接管 pipes，再发送 `Owned(request_id)`；
4. target stdout 只允许一个合法 `Event::Ready`；Owned 必须先于 Ready；
5. 第二个 Ready、日志污染、超限或伪造 Exit 都是 target protocol failure；
6. Ready 前 target exit 只发送一次真实 Exit，再清理残余 tree；
7. Ready 后 stdout EOF 不等于进程退出；exit authority 只来自 owner。

复用 `RequestSequence`：ID 必须严格增大但允许跳号；成功 decode 后系统调用失败仍消耗
ID；重复/倒序失败；`u64::MAX` 可接受一次且不 wrap。无法可靠取得 request ID 的 fatal
frame/parse 错误不得伪造 id=0 reply，只能关闭协议并清理。

每个有效 control 恰好一个 Reply；Ready/Exit 各最多一个异步 Event。

## 5. Stop、Exit 与 deadline

`Signal(false)` 幂等进入 graceful，重复请求不延长 deadline；Result 仅表示请求已接受。
`Signal(true)` 精确 force 整个 owned tree，false 不能降级它。`IsEmpty` 只使用 owner
证据；leader exit 不能替代 tree empty。Empty 后保留轻量 tombstone，Signal 幂等成功、
IsEmpty 恒 true，直到 parent stdin 关闭。

建议内部默认预算，测试可注入小值：Launch 10s、Ready 30s、write/flush 2s、graceful
5s、force drain 10s，owner poll 从 10ms 起；全部使用 monotonic clock。

自然 leader exit 时发一次 Exit，随后 force 残余 tree。parent EOF、broken pipe、writer/
ready timeout、fatal decode 和 cancellation 全部复用同一 stop state machine。输出失效时
不能等错误 reply 成功才清理；owner 只能由该 actor/cleanup path 消费。

正常 target 的 parent-EOF native test 必须在预算内回收。helper 自身被 SIGKILL 时，
POSIX 无法依靠 Rust Drop 保证子树回收；G8 不虚假宣称解决该 OS 故障边界。

## 6. 平台与 supervisor 约束

| 行为 | POSIX | Windows |
| --- | --- | --- |
| containment | generation-pinned process group | suspended spawn -> Job -> resume |
| exit observe | WNOWAIT，不提前 reap | non-consuming child observation，保留 Job |
| graceful | stdin EOF + 精确 SIGTERM | stdin EOF cooperation |
| force | owner SIGKILL path | exact Job kill |
| empty | leader+group 证据 | Job active process count=0 |

Windows `IsEmpty` 不用会分配 PID Vec 的 members API。禁止 `pkill`、`taskkill`、按名杀
进程、只杀 leader 或从外部 PID 重新 attach。

一个 helper/pipe connection 就是 generation authority。stale callback 只能影响原连接；
manual stop 不自动 restart；Unconfirmed 阻止 restart。backoff/max attempts 属于上层纯
supervisor policy。真实 OD、workspace mount、host lease heartbeat、capability broker、
handoff、pin/version 校验和 Node launcher EOF 行为都留 G2/G5/G10。

## 7. 错误与 redaction

继续使用现有四种 wire code：InvalidRequest、LaunchFailed、SignalFailed、Internal。stdout
只发送协议对象；stderr 只允许固定 reason/stage/request ID/count。禁止记录 raw frame、
token、cwd、executable、argv、env、child stderr 或 `io::Error.to_string()`。

Ready token 只在私有 pipe 与受限内存存在，不落盘；stop/failure 后清除可用引用，但不
声称普通 Rust String 具备密码学 zeroization。`LaunchSpec` Debug 必须改为脱敏。

## 8. 验收与切片

最低矩阵：frame/codec 不退化；容器放大和重复字段；Launch/ID/u64 边界；Owned-before-
Ready；Ready 缺失/两次/污染/超时；partial/broken/blocked writer；stderr flood；leader
exit+grandchild；graceful接受/忽略与force优先；各状态 parent EOF；cancel/probe/reap
failure；Unconfirmed；POSIX reuse；Windows descendant teardown；真实 stdio helper smoke。

按 [执行门禁](../EXECUTION.md) 顺序拆分：state/policy/error、bounded Launch decode、control reader、
single-slot writer、POSIX pipes/observe、Windows pipes/observe/Job empty、common owner seam、
Launch/Owned、Ready、dispatch/sequence、exit cleanup、deadlines、EOF/cancel 合流、redaction/
stderr、`run()` helper 接入、POSIX/Windows process smoke、aggregate SOL review。

G8 全部可与 G1 并行；共享 Cargo/lock 只允许单 owner 窗口。G8 PASS 仅表示私有 helper
能安全管理一个测试 target generation，不表示 Creative sidecar、workspace 或产品页面
可用。
