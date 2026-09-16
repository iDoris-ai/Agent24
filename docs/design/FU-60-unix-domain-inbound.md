# FU-60 —— 模块的入站监听改走 Unix 域套接字

> 状态：设计稿 v4（v1 → v2：4 Medium / 4 Low 全部采纳；v2 → v3：第 2 轮 1 Medium / 3 Low 全部采纳；v3 → v4：第 3 轮（终审）无 Medium+，4 Low 全部采纳，见文末——**设计通过，可以开始写代码**）。来源：`followups.md` FU-60；ME3-NEXT 执行队列第二段（用户已拍板：改走 Unix socket，不做「每模块连接新建速率上限」那个备选）。
> **协议变更**：`A24_LISTEN_FD` 这个环境变量名字和值（恒为 `3`）不变，但 fd 3 上的套接字族从 `AF_INET` 换成 `AF_UNIX`。SPEC-ME3-OUT-OF-PROCESS.md §1 的表格与 §397 需要同步改写（本设计稿末尾列出改动点）。
> **排期依赖（已解决）**：本刀依赖 FU-57（Supervisor 失败分类，`failure.rs`）已经落地。实现时 FU-57（#191）尚未合并，`open_generation()` 的失败暂时归为 `Stopped::Exited`（不是回归——`main` 上原本就是这样）；FU-57 合并（`606316c`）之后已 rebase 并接上 `failure::listen` 的 `setup` 分类，同时把它的文案改成中性——`open_generation` 一次覆盖两个 socket，从 `Result` 分不清哪一个失败了。

## 问题

`proxy.rs` 的上游连接只复用**无请求体**的请求（有请求体时无法确认模块读完，复用会被用来夹带请求，评审第 3 轮）。所以持续高速的**带请求体**短请求仍是一请求一连接、由代理先关，每个都在代理这一侧留一个 TIME_WAIT（Linux 60s），极端速率下可能耗尽回环临时端口（`127.0.0.1` 的临时端口范围是有限资源，且和本机其它进程共享）。

Unix 域套接字没有 TIME_WAIT（v4 措辞修正：它们支持 `shutdown` 半关闭，只是不实现 TCP 那一套需要 TIME_WAIT 的序列号/重传状态机），也不占用端口号命名空间——从这个特定失效模式的根上解决问题，而不是限速绕开它。

## 今天的形状（D4）

- `supervisor.rs::run_once` 在 spawn 之前 `std::net::TcpListener::bind("127.0.0.1:0")`，得到一个**每代新开**的端口（D4 的原意：避免一条连接被前一代排队或复用后打到新进程）。
- 这个 `std::net::TcpListener` 整个所有权经 `LaunchSpec.listener` 交给 `launch::spawn`，`command-fds` 把它的 fd 映射到子进程的 fd 3（`A24_LISTEN_FD=3`），随后**daemon 自己这一份 fd 被关掉**——「模块一死，它的端口立刻拒绝连接，而不是把请求堆进一个没人 accept 的 backlog」（`launch.rs` 已有测试 `once_the_module_is_gone_its_port_refuses` 覆盖这一点，TCP 版本）。
- 这个地址（`SocketAddr`）存进 `Generation.upstream: Option<SocketAddr>`（`drain.rs`），代理按代取地址转发（SPEC §397）；`proxy.rs::Upstream::connect(generation, addr)` 手工 `TcpStream::connect` 后做 hyper h1 handshake，按代做连接池（不按地址）。**`forward` 用这个地址的 `.to_string()` 合成请求的 `Host` 头**（`proxy.rs`，紧跟在头部过滤之后）——v1 漏看的一处，见下文第 6 节。

## 改成什么

### 1. 目录：复用 `CallbackDir` 的 `0700` 目录，一次性原子发号

**v1 → v2 的修正**：v1 拆出公开的 `next_generation()` 让调用方自己传号给 `callback_at(n)`，这重新打开了 `listen_at(n)` 原本被设成私有专门要堵住的口子——「调用方能选号」曾经允许同一个回调路径被 accept 两次（PR-Daemon 评审 #179 L1 就是这个教训，`listen_next()` 才把发号权收回目录内部）。v2 改成**一次性发一对**：

```rust
pub fn open_generation(&self) -> Result<GenerationSockets, EndpointError>
```

`GenerationSockets { n: u64, callback: CallbackListener, http_listener: std::os::unix::net::UnixListener, http_path: ModuleListenPath }`——号码在这一个方法内部产生并立即消费掉，两个 socket 要么一起绑定成功、要么第一个成功第二个失败时把第一个连带清理掉（复用 `CallbackListener`/`ModuleListenPath` 已有的 Drop）。**`listen_next()` 保留**，给只要回调 socket、不碰入站监听的调用点（今天大量端到端之外的握手测试）——它内部还是「发一个号、只用于 callback」，跟 `open_generation` 的号是同一个计数器，互不冲突（号码本来就允许有空洞，一个只发了回调没发监听的号不破坏任何不变式，只是那次没有对应的 `.l` 文件而已）。

入站监听 socket 落在 `<dir>/<n>.l`（**不叫 `.sock`**：`MAX_SOCKET_PATH` 是 103 字节的硬顶——macOS `sockaddr_un.sun_path` 长度——`.l` 只比裸数字多 2 字节）。**但这不增加整体可用的 `state` 路径预算**：只要一次真正的运行两个 socket 都要开，`<n>.sock`（回调，更长）依然是那个先触顶的成员，`.l` 只是省字节、不是省出新余量（v1 那句「留给 state 路径本身的余量越大」说得夸大了，见文末）。

### 2. 生命周期：这是本刀唯一容易做错的地方

**`CallbackListener`（回调 socket）的 Drop 语义不能照搬。** 它在 `accept_one` 取到唯一一次连接后**立即**关闭监听、删除路径——因为回调通道的全部使命就是把那一次连接引荐给内核，之后再也用不上这个名字。**模块的入站监听 socket 恰恰相反：它要在这一代存活期间被反复 `connect`**（每一次代理转发都是一次新连接），也就是说它可能在模块被创建后活好几个小时。如果照搬「Drop 时删路径」，而这个 Drop 在 daemon 侧的 `UnixListener` 值被丢弃时触发——**这个值恰恰在 spawn 之后几乎立刻被丢弃**（下一条）——那么模块进程还活得好好的、还在通过继承来的 fd 正常 `accept()`，但路径已经从文件系统里消失，后续所有 `Upstream::connect` 都会 `ENOENT`，相当于模块一起来就断供。这不是理论风险：`launch.rs` 已有的测试 `once_the_module_is_gone_its_port_refuses` 断言的就是「daemon 侧的这一份 fd 在 spawn 后立刻被关掉」，如果不拆开「关 fd」与「删路径」这两件事，改 Unix socket 的第一版几乎必然把这条测试改成绿的、同时让代理彻底连不上模块。

**结构：拆成两个不同寿命的东西**，且**把守卫和裸监听句柄一起塞进 `LaunchSpec`，跟随 spawn 的成败一路带到 `ModuleProcess`**（v1 → v2：v1 让 `run_once` 在 spawn 之后另外把守卫挂到 `ModuleProcess` 上，中间隔着「spawn 失败被 `?` 提前返回」这类分支，容易漏挂；改成守卫和监听句柄一起旅行，spawn 失败时两者一起被丢弃，天然无遗漏）：

- `http_listener: std::os::unix::net::UnixListener`——**裸的、可移动进 `LaunchSpec` 的监听句柄**（`std`，非 tokio：和今天 TCP 用 `std::net::TcpListener` 是同一层次，因为它只是被拿去做 fd 映射，daemon 自己不 `accept`）。`command-fds` 映射到 fd 3，`launch::start`（FU-58 拆出的、创建子进程那段同步 `fn`）返回后这个值超出作用域、daemon 侧的 fd 关闭——和 TCP 今天的行为完全一致，「模块一死立刻拒绝连接」的保证对 Unix socket 同样成立（关掉 daemon 侧最后一份指向该监听 socket 的 fd，是这条保证成立的**充分必要条件**，跟 TCP 还是 Unix 无关）。
- `ModuleListenPath`——**只存路径 + 节点校验（`(dev, ino)`）+ 目录 claim** 的守卫（字段形状和 `CallbackListener` 已有的一样，复用同一套「Drop 时只删除仍然指向自己的那个节点」的 best-effort 判据），**不持有 socket 本身**。它随 `LaunchSpec` 一起传进 `launch::start`，成功时被安放进新建的 `ModuleProcess` 的一个字段；`launch::start` 失败（`LaunchError`）时它和 `http_listener` 一起被丢弃、Drop 立即清理，不留孤儿文件。`ModuleProcess` 是「这一次运行的整个生命周期」的天然宿主：spawn 成功后创建，确认停止或放弃时 drop。**它的 Drop 才真正删除路径**——这时模块进程已经被确认停止（或至少不再被这个 `ModuleProcess` 管理），没有谁还会去 `connect` 这个名字。
- `Generation.upstream` 类型从 `Option<std::net::SocketAddr>` 改成 `Option<std::path::PathBuf>`（`ModuleListenPath.path()` 的克隆），`Generation::serving_at` 签名同步改；**`Generation::upstream()` 不再能按值返回**（`SocketAddr` 是 `Copy`，`PathBuf` 不是）——改成返回 `Option<&Path>`（借用生命周期够用的地方），调用方需要拥有值时显式 `.to_owned()`。**`Generation` 本身不持有 `ModuleListenPath`**——它是被代理层广泛 `Arc` 克隆、传阅的对象，把清理逻辑挂在它的 Drop 上意味着「最后一个引用它的地方在哪、什么时候掉」决定路径何时被删，这个时机不受 supervisor 控制。清理只应该由**知道进程真的没了**的那一方（`ModuleProcess`）触发。

**逐路径核对「`ModuleProcess` drop 时清理」是否会抢跑代理路由**（Codex 设计审查已走查，结论：不会，摘要如下，供实现时对照）：
- 正常停止：`finish` 先撤销（`begin_stop`）再发布 `Stopping`，`stop_then` 确认进程组清空才把 `stopped` 置真并返回；`ModuleProcess` 在这之后才被 drop，路径清理落在撤销**之后**——撤销已经让这一代拒绝新的准入，不存在「路径没了但还该被路由」的窗口。
- 排空（drain）：`ModuleProcess` 在整个 `drain_run` 期间原样存活，路径在排空窗口内保持可连——在途请求不受影响。
- 取消/panic/supervisor 被中止：`ModuleProcess::drop` 先撤销、再尝试 SIGKILL；Rust 字段析构在自定义 `Drop` 体**之后**按**声明顺序**进行（不是逆序——v2 这里说错了，第 2 轮审查指出）；但这不影响结论：不管 `ModuleListenPath` 这个字段声明在哪个位置，它的清理都发生在整个 `ModuleProcess::drop` 方法体（撤销 + 尝试 SIGKILL）跑完**之后**，因为自定义 `Drop::drop` 方法体本身就在所有字段析构之前完整执行完毕，声明顺序只决定字段之间彼此的先后，不决定它们相对于 `drop` 方法体的先后。
- `StopFailed`（`Unconfirmed`）：返回值继续持有 `ModuleProcess`，supervisor 记录失败后显式 drop 它；这一代已经撤销、代理不会再合法路由到它，进程组可能仍未确认清空，但没有请求会再合法地拨这个号——不是行为倒退（TCP 下这种情况本就没有「端口」可清理，Unix 版只是多了一步清理文件，客户端可见效果不变：仍然连不上）。
- 重启换代：新一代只在旧 `run_once` 彻底停下旧进程之后才开始；替换完成前槽位里可能还留着旧 `Generation`，但它已撤销、拒绝准入——不存在「`ModuleProcess` 已经不在了、但它的路径仍是唯一合法路由目标」的窗口。

### 3. 连接：`proxy.rs::Upstream::connect`

`TcpStream::connect(addr)` → `UnixStream::connect(path)`；去掉 `set_nodelay`（Unix 域套接字没有 Nagle 算法，这一步对它是无操作，直接省略而不是保留一个无意义的调用）。其余（hyper h1 handshake、按代复用池、撤销时立即关闭空闲连接）**完全不变**——`Upstream` 结构体和 `IdleConnections` 都不关心地址的具体形状，只关心「哪一代」。

### 4. 模块这一侧：几乎不用改

Python 的 `socket.socket(fileno=fd)`（不传 `family=`）会用 `getsockopt(SO_DOMAIN)` 自动探测出 `AF_UNIX`（已用一个 socketpair 验证过：`family == socket.AF_UNIX`），后续 `.accept()` / `.sendall()` / `.close()` 在字节流语义上和 TCP 完全一样。这正是 systemd socket-activation 约定本来的设计目的——遵循这个约定写的代码本就不该关心 fd 背后是什么族。核对过仓库里三处实际拿 `A24_LISTEN_FD` 当纯 fd 用的 Python fixture（`launch.rs`、`agent24d/src/domain.rs`、`agent24d/tests/daemon_modules.rs`）——都只是 `socket.socket(fileno=int(...))` 后 `accept()`/收发字节，不看对端地址形状，**不需要改一行**。`proxy.rs` 里模仿"模块"的测试夹具不是 Python，而是 Rust + axum 起的 TCP 服务器，这些**需要机械改造**成 `axum::serve(UnixListener, app)`（axum 0.8 原生支持）；其中若有测试靠对端 `SocketAddr` 做「按连接计数/区分」的断言（例如 `ConnectInfo<SocketAddr>`），Unix socket 客户端通常拿不到有意义的对端地址，这类断言要换成别的区分手段（例如按 accept 到的连接实例本身计数，而不是按对端地址分组）。

### 5. `MAX_SOCKET_PATH` 与两种 socket 挤在一个目录

`endpoint.rs` 的 `MAX_SOCKET_PATH = 103` 今天只为回调 socket 把关。`.l` 后缀比 `.sock` 短 3 字节，但**两条命名空间共用同一个上限判据**（`open_generation` 内部对两个路径都做同一个 `path.as_os_str().len() > MAX_SOCKET_PATH` 检查，不新开一个更松或更紧的常量）；因为 `.sock`（回调）总是更长，它才是真正卡预算的那个——`.l` 更短这件事本身不释放任何额外的 `state` 路径空间，只是保证「两者之中先触顶的一定是回调，不会出现监听 socket 建得成、回调建不成的反常局面」。**入站监听 socket 的节点也要 `chmod 0700`**（v4，终审 Low）：`endpoint.rs::listen_at` 今天对回调 socket 节点有这一步（防 umask 泄权），`open_generation` 对 `.l` 节点要显式做同样的事，不能假设「同一个目录已经是 0700」就足够——节点自身的权限位是独立的一层。

### 6.（v2 新增）代理转发合成的 `Host` 头不能来自文件系统路径

**v1 完全漏看的一处**：`proxy.rs::forward` 在过滤请求头之后，用 `upstream.to_string()`（今天是 `SocketAddr`，形如 `127.0.0.1:54321`，本身就是合法的 HTTP authority）合成 `Host` 头写回请求。换成 `PathBuf` 之后，`.to_string()` 会是一个文件系统路径（可能含非 UTF-8 字节、且不是合法的 HTTP authority），既不正确也**向模块泄露了内核状态目录的本地路径**。

**改法**：`Host` 头改成一个**固定的合成值**：`agent24-module.invalid`（`.invalid` 是 RFC 2606 保留的、明确不可解析的顶级域，比裸造一个名字更清楚地表明「这不是一个真实网络位置」），与连接机制彻底解耦——不管上游是 TCP 地址还是 Unix 路径，模块看到的 `Host` 永远是这同一个值，文档里写明这是内核合成的、不代表任何真实网络位置。这不是 FU-60 才引入的新问题（今天用 `SocketAddr` 拼出的 `Host` 其实也只是一个内部实现细节被意外泄露给模块，只是恰好长得像合法值、没人注意），FU-60 顺手把它钉死成显式、稳定的契约。

## 判据（带正对照）

1. **压测**（followups.md 原判据）——收窄为**确定性断言而非计时型 soak 测试**：验证代理的上游连接与继承的入站 fd 都是 `AF_UNIX`（这是消除 TCP TIME_WAIT 的实质证明，不依赖机器快慢）；一个真正的高并发计时压测（2000 req/s × 60s）作为**人工/按需**的补充验证，不进 CI 强制判据（环境敏感，参考 SHUT 系列判据对「计时类判据」的一贯处理方式）。
2 + 3（合并，按正确的时序拆开，原 v1 #2 与 #4 互相矛盾——`stop()` 成功会让 `ModuleProcess` 连同路径守卫一起消失，此时再 `connect` 应该是 `NotFound` 不是 `ConnectionRefused`，顺序不能像今天 TCP 测试那样「先 stop 再 connect」）：
   - **daemon 侧 fd 立刻关闭**：spawn 一个短命模块，等它的首领退出、但先**不** `stop()`（`ModuleProcess` 还在，守卫还在）；断言此刻 `.l` 路径仍然存在，且 `connect()` 得到 `ConnectionRefused`（不是挂起、不是 `NotFound`）——证明 daemon 没有多留一份 fd，模块一死立刻拒绝，路径本身却还没被清理。
   - **路径在停止后被清理**：接着 `stop()`；断言路径消失，此后 `connect()` 得到 `NotFound`。
   两步合在一起，把「关 fd」与「删路径」两件事分别钉死在各自的判据上：变异——把清理逻辑挪回「daemon 侧 `UnixListener` drop 时删路径」——会让第一步里路径提前消失，测试变红。
4. **路径在整段生命周期内持续可连**（正面场景，防「本设计要防的那个 bug」）：模块 spawn 后，daemon 侧的裸监听句柄已经离开作用域，模块仍在正常运行；`Upstream::connect` 走完至少两次独立的请求-响应（不是一次，避免巧合）。
5. **两个 socket 同号**：`open_generation()` 返回的 `n` 是两个路径文件名共同的数字前缀——直接从各自的 `path()` 解析比较，不是靠“看起来差不多”。
6. **`MAX_SOCKET_PATH` 边界**（v3 修正：v2 把「103 字节」和「超限」搞反了——`MAX_SOCKET_PATH = 103` 是**允许**的上限，104 才拒绝）：构造一个目录路径，使 `<dir>/<n>.sock`（回调）恰好等于 103 字节——`open_generation` 必须**整体成功**，两个 socket 都建出来；再加长 1 字节使其恰好 104 字节——此时必须**整体失败**（哪怕 `.l` 那一路径本身没超限），且不留下任何一半打开的 socket（这条断言的是「一起成、一起败」，不是两条独立判据）。
7.（v3 新增）**共享计数器的跨 API 回归**：依次调用 `listen_next()`、`open_generation()`、`listen_next()`，三次拿到的编号两两不同，且三个（组）监听器全部仍然存活——防的是「两个方法各用一个独立计数器」这种表面上跑得通、实际上编号会撞车的实现。
8.（v3 新增）**部分绑定失败时不留孤儿、不误删无关文件**：预先在 `<dir>/1.l` 占位绑定一个无关 socket，再调用 `open_generation()`（它会先成功建出 `1.sock`，随后在 `1.l` 上绑定失败，因为名字被占）；断言：新建的 `1.sock` 被清理掉，而预先占位的 `1.l` 完好无损（不是被顺手清空重绑）。
8b.（v4 新增）**spawn 失败时 `.l` 路径自动清理，不需要调用方手动善后**：两个 socket 都绑定成功，随后 `launch::spawn`/`launch::start` 本身失败（例如命令解析不了）；断言 `.l` 文件从磁盘上消失——这条钉住的正是「`ModuleListenPath` 跟 `LaunchSpec` 一起旅行」这个结构性设计（第 2 节），而不是指望某处显式调用清理逻辑。
8c.（v4 新增）**`.l` 节点权限**（对照回调 socket 已有的同类测试）：`open_generation()` 建出的 `.l` 节点 mode 恰好是 `0700`，不是 umask 决定的默认值。
9. **`ModuleListenPath` 的 best-effort 节点校验**（对照 `CallbackListener` 已有的同类测试）：路径被别的东西抢先复用（同名文件已存在，节点不同）时，Drop 不删除它——不是「删除失败」，是「确认那不是我绑定的那个节点，不删」；判据构造：先正常创建并记住其 `path()`，unlink 该节点后在同一路径上绑定一个新 socket（模拟被别的东西抢占），再让原来的守卫 drop，断言新 socket 的文件仍在。
10.（v2 新增）**`Host` 头是固定合成值，不泄露路径**：代理转发到 Unix socket 的请求，`Host` 头等于文档写明的固定值；断言这个值里**不含**状态目录路径的任何片段（例如不含 `.agent24`、不含 daemon pid、不含 `.l`）。

## 不改的东西

- `RestartPolicy`——`open_generation` 失败沿用**当时 `main` 上的**失败分类路径（如果 FU-57 已落地：`failure::listen`/`failure::bind`，按阶段分类，`match` 分支覆盖 `EndpointError`/`std::io::Error` 的全部来源，不按套接字族区分——绑定调用从 TCP 换成 Unix，分类函数的**分支**不用改）。**但诊断文案要改**（v3，第 2 轮 Low）：`open_generation()` 对外只暴露一个 `Result<_, EndpointError>`，调用方分不清失败的是回调 socket 还是入站监听 socket；`failure::listen()` 今天的文案硬编码了「callback」，`failure::bind()` 只接受裸 `io::Error`——两者都要把文案改成中性的「generation endpoint」/「the generation's socket」之类，不点名是哪一个，或者让 `EndpointError`/`open_generation` 的返回类型保留「这是哪一路失败的」这条信息，分类结果（`Setup`）不变，只是说出来的话要准确。**这条要做彻底**（v4，终审 Low）：`EndpointError::Io` 自己的 `Display` 实现今天就写死了 `"callback endpoint: {e}"`（`endpoint.rs`），不是只有 `failure.rs` 那层包装文案硬编码——`EndpointError` 本身也要改成中性文案，否则套着中性包装、里面还是印着「callback」，`.l` 的失败照样被说成回调失败。
- 代理的复用/撤销/请求体处理策略（SPEC §397 的绝大部分文字）——只有「地址」的具体类型变了，复用按代不按地址的规则、带请求体不复用的规则，一个字都不用改。
- `A24_LISTEN_FD` 的名字、值（`3`）、以及"跳板"/fd-隔离（只有 0-3 到达模块）那一整套机制——`command-fds` 对 `AsRawFd` 的任何实现都一视同仁，`UnixListener` 和 `TcpListener` 在这一层没有区别。

## SPEC-ME3-OUT-OF-PROCESS.md 需要改的地方

- §1 表格「入站代理」一行：「内核预开监听 socket，把 FD 传给子进程」后面补一句「Unix 域套接字，路径与回调 socket 同一个 `0700` 目录」，去掉隐含的"就是 TCP"的默认印象。
- §397「每一代都记着自己进程的监听地址」→ 改成「监听路径」；「spawn 时交给进程的那个新端口（D4）」→「新 socket 路径」；补一句「转发给模块的 `Host` 头是内核合成的固定值，不是这个路径」。
- 新增一句：入站监听 socket 与回调 socket 共享同一个 `<state>/run/<pid>/` 目录、同一次原子发号（`open_generation`），前者后缀 `.l`、后者 `.sock`。

## v1 → v2（Codex 设计审查采纳）

- Medium 1：`next_generation()` + 调用方传号的两步式 API 重新打开了「调用方能选号」的口子（PR-Daemon #179 L1 教训）→ 改成一次性原子发号的 `open_generation()`，返回配对的 `GenerationSockets`。
- Medium 2：代理转发合成 `Host` 头目前来自 `upstream.to_string()`，换成路径后既不合法也泄露本机路径 → 改成固定合成值（新增第 6 节 + 判据 8）。
- Medium 3：原判据 #2（先 `stop()` 再 connect 期待 `ConnectionRefused`）与 #4（`stop()` 后期待路径消失、`connect` 应为 `NotFound`）互相矛盾 → 拆成两步时序正确的判据（新 #2+#3）。
- Medium 4：「不改的东西」引用了写作时 `origin/main` 上还不存在的 `failure.rs`/`FailureKind`（FU-57 当时未合并）→ 顶部加排期依赖说明，措辞改成「FU-57 落地后的样子」。
- Low：`ModuleListenPath` 改为随 `LaunchSpec` 一起旅行到 `launch::start`，而不是 `run_once` 事后再挂——spawn 失败/取消的路径不会遗漏清理。
- Low：`.l` 更短不等于「多出可用的 state 路径预算」，措辞改正（第 1、5 节）。
- Low：`Generation::upstream()` 不能再按值返回（`PathBuf` 非 `Copy`），改成返回 `Option<&Path>`。
- Low（未采纳，记录原因）：Codex 建议把 `CallbackDir` 改名成中性的 `EndpointDir`/`SocketDir`。本刀不做这个改名——它是纯重命名、不影响本刀的正确性论证，且会扩大这个已经不小的 diff 的 blame 范围；留给愿意做这件事的人单独提，不搭在 FU-60 上。

## v2 → v3（第 2 轮 Codex 设计审查采纳）

- Medium：判据 6 把 `MAX_SOCKET_PATH` 的允许/拒绝边界写反了（103 字节应该**成功**，104 才失败）——改正，并把「共享计数器」「部分绑定失败的清理」两条本该有、但 v2 没写出来的判据补上（新判据 7、8）。
- Low：v2 里「字段按声明逆序析构」的说法是错的（Rust 字段按声明顺序析构；逆序析构的是局部变量）——改正措辞，结论不变（`ModuleListenPath` 的清理不管字段声明在哪都晚于 `Drop::drop` 方法体本身跑完）。
- Low：`Host` 固定值从「例如 `agent24-module`」定成明确的 `agent24-module.invalid`（`.invalid` 是 RFC 2606 保留的不可解析顶级域）。
- Low：FU-57 的 `failure::listen()`/`failure::bind()` 文案硬编码了「callback」，`open_generation()` 失败时说不清是回调还是入站监听 socket——文案要改成中性措辞（分类结果 `Setup` 不变）。

## v3 → v4（第 3 轮/终审 Codex 设计审查采纳；无 Medium+，设计通过）

- Low：v3 只改了 `failure.rs` 那层包装文案，`EndpointError::Io` 自己的 `Display` 还硬编码「callback endpoint」——两层都要改中性，否则包装是中性的、里面还是印着「callback」。
- Low：补一条判据（8b）钉住「spawn 失败时 `.l` 自动清理」——这正是第 2 节「守卫随 `LaunchSpec` 旅行」这个结构性设计要保证的事，此前只测了「部分绑定失败」（8），没测「两个都绑成了、但 spawn 本身后来失败」这条更贴近真实故障的路径。
- Low：`.l` 节点也要 `chmod 0700`（对照回调 socket 已有的同类保护），补判据 8c。
- Low（措辞）：「Unix 域套接字没有半关闭状态机」不准确——它们支持 `shutdown()` 半关闭，只是不需要 TCP 那一套为 TIME_WAIT 服务的序列号/重传状态机，改正措辞，结论（没有 TIME_WAIT）不变。
