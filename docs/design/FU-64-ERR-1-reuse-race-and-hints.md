# FU-64 + ERR-1 —— 连接复用撞上模块关闭；连不上模块的应答都带得上手的解决办法

> 状态：**定稿 v6**（v1→v2 第 1 轮 0/7/6/0；v2→v3 第 2 轮 0/3/1/0；v3→v4 第 3 轮 0/2/1/1；v4→v5 第 4 轮 0/0/1/0；v5→v6 第 5 轮 0/0/3/2；第 6 轮 0/0/0/0，独立通读全文无新发现，判「clean, ready to code」——见文末）。累计 6 轮 Codex 设计审查，0 Critical / 12 High / 12 Medium / 3 Low，全部采纳。可以开始写代码。来源：`followups.md` FU-64（2026-09-13 裁决：幂等重发 + 空闲上限 + 结构化提示三件一起做，ERR-1 同批）；`tasks.md` ME3-NEXT 执行队列第三段。承接 FU-63（PR #184，已把「取回未发出的请求」那条路径钉住，本设计不重做）；承接 FU-57 末尾的留白：「不加新的 API 字段：结构化错误码与解决办法属于 ERR-1，那时统一设计」——本设计就是那次统一设计。

## 问题

`exchange()`（`agent24-os-proto/src/proxy.rs:613`）复用一条池里的连接时，模块可能在任意时刻关闭它。两个窗口，行为不一样：

- **窗口 (a)：`take()` 之后、`try_send_request` 真正把字节写出之前**关闭。hyper 看得出请求没发出去，`try_send_request` 的 `Err` 里带回原始请求（`e.take_message() == Some(unsent)`），`exchange()` 已经把它接住、换新连接重发（FU-63/PR#184 的产出）。**这条路径今天没有专门的确定性测试**——`a_module_closing_an_idle_connection_costs_no_502` 钉住的是「关闭已经可见」（`take()` 时的存活过滤）这一半，过滤和重发两条防线中拿掉任意一条都会变 502，但两条各自单独覆盖到窗口 (a) 本身的证据只有注释。
- **窗口 (b)：hyper 已经把请求交给内核 socket 之后**才关闭。`take_message()` 返回 `None`——hyper 自己也不知道模块收没收到——`exchange()` 今天原样把错误字符串网上抛，`forward()`（`proxy.rs:954-960`）把它折叠成通用的 `502 upstream_unavailable`，消息只有 hyper 的原始错误文本，没有结构化的 `code`，没有「这是什么意思、该怎么办」。**这是 FU-64 真正要解决的缺口**——(a) 已经有防线只是缺测试，(b) 今天完全没有处理。

`sequential_requests_to_one_generation_reuse_a_connection`（`proxy.rs:2817`）等既有测试跑的是「模块乖乖等」的正常路径，没有一个测试模拟「模块在每次应答后立即关连接」——FU-64 的判据（2000 个连续 GET 零 502）需要这样一个夹具，今天也没有。

配合 ERR-1：客户端打到一个「连不上模块」的模块时，**除了 `Starting`/`Draining`/`Revoked` 这三种一般性的生成代状态**（`RequestRefused::{NotReady,Draining,Stopping}`，`drain.rs:134-156`），实际上模块的插槽（`Current`，`drain.rs:503`）没有活代时，背后可能是六种完全不同的终态（`supervisor::Status`：`GaveUp` 熔断器跳了、`PackageChanged` 包被换掉、`StopFailed` 停止本身失败、`Panicked` 监督者自己崩了、`Killed` 被直接杀掉没走正常停止、`Stopped` 正常停止；`Backoff` 退避中不算，见 D 段），今天全部折叠成一句 `module_stopping` / "the module has been stopped"（`proxy.rs:801-812`）。运营者在 `agent24 os list` 上能看到区别（`os_routes.rs` 的 `detail` 文案），但**打这个模块命名空间的请求本身看不出来**——FU-61 判据 6 明确记了这个回归：「`PackageChanged` 状态下打到该模块的请求仍然是既有的 503 `module_stopping`」，当时特意没有改，留给 ERR-1。

两个问题拢共有一个共同的协议缺口：`ErrorBody`（`agent24-protocol/src/types.rs:176`）只有 `code` / `message` / 一个从未用过的 `details: Option<Map>`，没有「这是什么状况、该做什么」这两件事分开表达的字段；CLI（`agent24-cli/src/main.rs:534,606`，两处，第 391 行不解析响应体，见「CLI」小节的核实）今天原样打印 `error.message`，没有第二个字段可打。

## 决策记录（用户 2026-09-13 已裁决，不重新讨论）

FU-64 的三件事一起做，不是三选一：① 窗口 (b) 对幂等、无请求体的请求换新连接重发一次；② 复用连接的空闲上限短于常见的模块 keep-alive；③ 重发用尽之后剩下的 502 带结构化提示。ERR-1 与 FU-64 同批，理由是两者都要改 `ErrorBody`——分两次改协议是不必要的折腾。

## 设计

### A. 窗口 (b)：幂等请求的真重试

`forward()` 已经把 `method`（`proxy.rs:850`）、经过消毒的 `headers`、`upstream_uri`、以及已经读满内存的 `body: Bytes`（`reusable = body.is_empty()`，`proxy.rs:861`）分开持有——`Bytes` 克隆是引用计数，`Method`/`Uri`/`HeaderMap` 也都是 `Clone`，所以重建第二个一模一样的 `Request` 不需要克隆 `Request` 本身（`http::Request` 没有 `Clone`），只需要把这几样重新交给同一段 `Request::builder()...` 逻辑跑第二遍。

**v1 → v2（Codex High 1）**：v1 的 `Unreachable`/`MaybeDelivered` 两分法按「走的是复用连接还是全新连接」分类，这是错的——一条全新连接的 `send_request()` 同样可能在**握手成功之后**才失败（模块在我们写完请求的同一瞬间关闭了刚建好的连接），这时候和「复用连接被取回未发出的请求」是两回事，却被 v1 的命名混在一起：一个说的是「有没有可能已经送达」，另一个说的是「这次尝试走的是哪条连接」，两个维度不该合成一个二分。改按**送达确定性**分类，不看连接是新是旧：

```rust
enum ExchangeError {
    /// 这次尝试确定没有送达任何字节：要么 `try_send_request` 明确退回了
    /// 未发出的请求（复用连接，已经在 `exchange()` 内部换新连接重发过、
    /// 仍不成功），要么全新连接在握手阶段（`connect()`）本身就失败——两种
    /// 情况下都不存在一个「模块曾经收到过这个请求」的世界线。
    NotSent(String),
    /// hyper 无法排除模块已经收到：复用连接的 `try_send_request` 报错但
    /// 拿不回原始请求（`take_message() == None`），或者一条握手已经完成的
    /// 连接（复用的或刚建好的）在 `send_request()`/`try_send_request` 阶段
    /// 失败——socket 层「写成功」不等于「对端读到」，握手完成过，就不能
    /// 排除模块已经动手。
    MaybeSent(String),
}
```

`exchange()` 内部：复用连接 `try_send_request` 失败且 `take_message() == Some(unsent)` 时，照旧换连接重发（FU-63 的既有行为，对调用方透明，不产生 `NotSent`/`MaybeSent` 中的任何一个——真正「没发出去」不算一次可观测的尝试）；这之后无论是复用连接报「拿不回」还是全新连接的握手/发送失败，都按上面两条规则分类，不再以「这段代码是不是全新连接那条分支」作为判据。

`forward()` 里：请求是否满足「幂等、无请求体」（`method` 是 `GET`/`HEAD`/`OPTIONS` 且 `reusable`）在读 body 之后立刻判一次，存成 `retry_eligible: bool`。`exchange()` 第一次失败若是 `MaybeSent` 且 `retry_eligible`：

**v1 → v2（Codex High 2）**：v1 说重试「不强制建新连接」——错误，那样会让唯一一次重试有概率又抓到另一条同样已经过期的池中连接，白白重试一次却没有真正排除歧义，和设计初衷矛盾。改成：重试固定走一个**跳过连接池**的路径（`exchange()` 加一个 `force_fresh: bool` 参数，或者拆出一个不查 `IdleConnections` 的姊妹函数），保证这一次尝试用的是刚建好的连接，把「连接是不是已经不新鲜」这个变量从重试里排除掉。

**v1 → v2（Codex High 3）**：v1 完全没提重试前要不要再看一眼撤销——这是个真实的新增竞态，不是把旧代码抄一遍就有的问题：今天只发生一次物理发送，`forward()` 在发送前调用一次 `in_flight.dispatch()`（`drain.rs:609`，检查 `DrainState` 不是 `Revoked`）就够了；FU-64 一旦引入「同一个请求两次物理发送」，第一次发送之后、重试发送之前的这段时间里，生成代完全可能被撤销（模块开始停止），而外层 `proxy()` 的 `tokio::select!`（`proxy.rs:764-767`）只在 `forward()` 整体返回时才有机会看到撤销信号——重试如果不重新检查，会在撤销之后还往一个正在停止的模块补发一次物理请求。**重试发送之前必须再调用一次 `dispatch()`**——核实过它的语义：`dispatch()` 做的是往 `InFlight` 记一次已发送标记（`drain.rs:609`），重复调用是安全的幂等操作，不用另找一个只读检查。检查失败（生成代已撤销）就放弃重试，不再发第二次物理请求。

**v2 → v3（Codex 第 2 轮 Medium 1）**：v2 这里写「检查失败就……把这次失败按 `MaybeSent` 交给 C 段」，和判据 8 的预期（撤销路径应该赢）自相矛盾，而且是错的——核实 `proxy()` 的外层逻辑（`proxy.rs:774-798`）：只要生成代在这次请求处理期间被撤销过，`in_flight.finish()` 就会返回 `Err(Abandoned{dispatched: true})`（`dispatch()` 在第一次尝试之前就已经调用过一次，所以「已经标记为发送过」这件事从第一次尝试起就成立），而这个返回值是**权威判定**、不依赖 `forward()` 内部算出了什么——`proxy()` 拿到它之后，无论 `forward()`/`exchange()`/C 段算出的是一个正常响应还是 C 段的结构化 502，一律丢弃，改报 `503 request_abandoned`（`proxy.rs:791-796`）。也就是说「重试前撤销检查失败」这条路径的最终客户端应答**永远**是 `request_abandoned`，不是 C 段的 `upstream_connection_closed`——不需要额外代码去保证这一点，`finish()` 的既有逻辑已经保证了；这里只是把文字说清楚，不是新加一条分支。`forward()` 在这个分支里返回什么内容因此并不重要（它会被丢弃），但为了内部逻辑一致，仍然按 `MaybeSent` 记录（毕竟第一次尝试确实可能已经送达），只是要在文字里明说这条路径「在客户端看来」永远走不到 C 段。

重试（在满足上述两条前提下）仍然失败——无论是 `NotSent` 还是又一次 `MaybeSent`——都落到 C 段。C 段如何解读「重试之后的最终结果」见下文，**不是简单取最后一次尝试的分类**（Codex High 1 的另一半）。不再重试第三次（判据要的是「零 502」，不是「重试到天荒地老」；两次不同的连接都撞见同一模块的即时关闭，说明这不是运气问题）。

**不做**：非幂等或带请求体的请求，窗口 (b) 失败不重试——模块可能已经执行了副作用，重发是把「未知」变成「已知的错误」，用户裁决已经把这条路径的出口定成 C 段的结构化 502，不是重试。

**信任边界（Codex Medium 2，新增小节）**：GET/HEAD/OPTIONS 的自动重试建立在 RFC 9110 §9.2.2「安全方法」之上——协议要求这些方法不应该有副作用，所以重试在协议层面是允许的。这个设计信任模块遵守这条约定；一个把状态变更塞进 GET 处理器的不合规模块，会在窗口 (b) 被重试时暴露出这个既有的不合规问题（而不是被这次改动新引入一个问题）。本设计不为这种不合规模块提供选择退出的开关——如果将来需要，是一个独立的、按模块声明的 followup，不在这次范围内。

### B. 复用连接的空闲上限

`IdleConnections`（`proxy.rs:562`）今天只有数量上限（`MAX_IDLE_CONNECTIONS = 8`）和存活过滤（`take()` 里的 `!c.sender.is_closed()`），没有时间维度——一条连接可以在池里等到天荒地老，直到模块自己的 keep-alive 计时器把它关掉，之后第一次复用才发现（落进窗口 (a) 或 (b)，取决于运气）。

给 `Upstream` 加一个 `idle_since: Option<Instant>` 或者 `IdleConnections` 把 `Vec<Upstream>` 换成 `Vec<(Instant, Upstream)>`（哪种更省事交给实现时决定，不影响外部行为）：`put()` 记下入池时刻；`take()` 除了今天的存活过滤，再加一条——`elapsed >= IDLE_MAX_AGE` 的连接视同已死，直接丢弃（`drop`，让 `UpstreamConnection` 的析构照旧结束 driver），不进候选。

`IDLE_MAX_AGE` 取一个比「常见的模块 keep-alive」短的经验默认值（不是正确性边界，见下方 v2 订正）。已知的几个参照：Node.js `http.Server` 默认 `keepAliveTimeout` 5000ms；Actix-web 默认 `keep_alive` 5s；两者是本仓库模块最可能使用的运行时（Cos72/Sin90 生态偏 Node）。提案默认 **`IDLE_MAX_AGE = 4000ms`**，可调（照 SHUT-1b 的先例，`A24_MODULE_IDLE_CONN_MAX_MS`，非法值告警回落默认，不拒绝启动）。

**v1 → v2（Codex Medium 3）**：4000ms 是一个启发式经验值，不是正确性边界，措辞改正——本段不再说「需要一个……默认值」，而是明说：**这是尽力而为的优化，不是任何正确性保证**。任意第三方模块完全可能用 0（每次都 `Connection: close`）、1s、60s，或者干脆没有固定 keep-alive；4000ms 只是「比两个常见运行时的默认值短一点」的经验猜测。真正的正确性保证来自 A/C 段（无论空闲了多久，撞上窗口 (b) 都有重试或结构化提示兜底）；B 段只是减少「一条已经死了很久的连接白占一次 `take()`」的频率。v1 提出的「按模块单独配置」留作 followup，不在这次范围——一个全局常量已经能覆盖大多数情况，模块级配置需要先有模块级配置的载体（目前没有），是独立的一次改动。

**v1 → v2（Codex Medium 4）**：新增的环境变量若只加进 `Timings`/常量定义，daemon 以 launchd 方式安装时会读不到它——`agent24-cli/src/service.rs` 的 `PASSTHROUGH_VARS`（目前 10 项）是 launchd plist 生成时唯一会转发给 daemon 进程的环境变量白名单，不在这张表里的变量在 CLI 前台跑没事、装成服务就静默失效。**`A24_MODULE_IDLE_CONN_MAX_MS` 必须同时加进 `PASSTHROUGH_VARS`**，照抄 `A24_MODULE_DRAIN_MS`/`A24_MODULE_STOP_GRACE_MS`（SHUT-1b）已经走过的两处改动（常量解析 + 白名单登记）。仓库里已经有一个扫描源码校验「新 `A24_*` 变量是否进了白名单」的测试（SHUT-1b 引入），本设计不需要新写这项检查，改代码时会被它捉住——但设计本身要先把这一步写进「改成什么」，不能只改常量定义就当作完工。

这一条**不改变 FU-64 判据要测的「每次应答后立即关连接」场景**——那种模块的连接一入池就已经是「随时可能已关」，空闲多久都一样，靠的是 A 段和 C 段；B 段解决的是更常见的「模块有真实但比池想象的短的 keep-alive」场景，两者互补，不是同一件事的两种写法。

### C. `exchange()` 的错误分叉与结构化 502

**v1 → v2（Codex High 1 的后半）**：v1 只看「重试之后最后一次尝试的分类」来决定 C 段的应答，是错的——如果第一次尝试是 `MaybeSent`（模块可能已经收到、可能已经执行），重试的第二次尝试哪怕明确是 `NotSent`（比如这次连接被拒），也不能因为「最后一次确定没发出去」就报成通用的 `upstream_unavailable`：第一次那个「可能已经发生的副作用」并没有因为第二次尝试失败就消失。改成**只要序列里出现过一次 `MaybeSent`，最终结果就是 `MaybeSent`**（不可逆——一旦「可能已发生」就不能被后续更确定的失败洗白）；只有从第一次到最后一次（含 A 段的那一次重试）全部都是 `NotSent`，才报「确定不可达」。

`forward()` 里 `exchange()`（以及 A 段可能追加的一次重试）失败的落点（`proxy.rs:954`）按上述规则分叉：

- 全程都是 `NotSent` → 维持今天的 `502 upstream_unavailable`，消息不变（这是模块整个不可达，不是「关连接的竞态」，不应该被 FU-64 的新 code 稀释掉，运维已经认得这个 code）。
- 序列里出现过一次 `MaybeSent`（无论是不 `retry_eligible` 直接落到这里，还是 A 段重试用尽）→ 新 `502`，`code: "upstream_connection_closed"`，`message`「模块在请求发出的同时关闭了连接；这个请求可能已被处理，也可能没有」，`hint`「确认无副作用后重试；频繁出现请调大模块的 keep-alive」——原文照抄 `followups.md`/`tasks.md` 的判据措辞，不重新组织语言，减少「hint 说的和任务描述不一致」这种评审来回。

### D. ERR-1：`Current` 的插槽状态也讲清楚

`Current`（`drain.rs:503`）只存一个 `Arc<Generation>`，`Generation` 只知道 `Starting`/`Draining`/`Revoked` 三态（`drain.rs:8` 的状态图）——插槽为什么没有活代（还在退避 / 熔断了 / 包换了 / 停止本身失败了 / 就是正常停了）这件事，答案在 `supervisor::Status`（`supervisor.rs:107`），今天只有 `SupervisorHandle::status()`/`subscribe()`（`supervisor.rs:181-197`）能读到，`Current` 拿不到。

**不新增状态机**（不碰 `Generation`/`DrainState`，不碰 `Status` 的任一变体或迁移）——只是把已经存在的 `Status` 读法接到 `Current` 旁边，让 `proxy()` 在要返回 `RequestRefused::Stopping` 的时候，多看一眼「上一次这个插槽的监督者为什么放手」：

**v4 → v5（Codex 第 4 轮 Medium 1，统一的「重启 daemon」措辞）**：下表和 E 段一共有六处 hint 建议「重启 daemon」——v5 曾经数成五处，第 5 轮 Codex Medium 1 指出数错了，见下方 v6 订正。v4 只把托管感知的说法用在了 `GaveUp`/`Stopped` 上，其余几处要么抄了 FU-61 时代带 launchd 脱管风险的旧文字，要么只写一句裸的「需要重启 daemon」——同一个仓库、同一个已知风险（`followups.md` FU-68），多处给出不一致、其中大半仍然不安全的建议，不是一个可接受的状态。

**v5 → v6（Codex 第 5 轮 Medium 1+2，订正计数、给出可执行命令、改成共享常量）**：

- **计数订正**：真正建议重启的是**六处**，不是五处：① `GaveUp`；② `Stopped`；③ `PackageChanged`；④ D 段的 `StopFailed`（代理路径）；⑤ E 段 `patch_os` 的控制面 `stop_failed`；⑥ E 段 `stop_now_os` 的控制面 `stop_failed`。v5 的文字和判据 11 都写成「五处」，是把④和⑤/⑥算重了——本节及判据 11 一律改成「六处」。
- **命令可执行**：v5 的句式里 `launchctl kickstart -k <label>` 的 `<label>` 是占位符，照抄不能跑——核实 `service.rs:19` 的 `LABEL = "ai.auraai.agent24"` 和 `service.rs:179` 组装 target 的方式（`gui/{uid}/{LABEL}`），以及 CLI 的 `agent24 service status` 实际打印的字段（`main.rs:929-931`，`loaded: yes`/`loaded: no`，不是抽象的「是否托管」）之后，标准句式改成给出真实可以照抄执行的完整命令：

  > 「重启 daemon——先 `agent24 service status`：`loaded: yes` 就用 `launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24`；`loaded: no` 才用 `agent24 daemon stop && agent24 daemon start`」

- **不要求逐字相同，要求共享同一个常量**：v5 说六处「完全一致」——这本身不成立：`Stopped` 前面还要加「先确认 enable」，`GaveUp` 前面还要加「运行 `agent24 os list` 看失败原因」，各自的前缀/后缀天然不同，「完全一致」这条要求本身是错的。真正要保证的不是完整字符串相同，是**这条重启指令本身**（上面引号里的那句）在六处都逐字相同、来自同一个来源，不是六份手抄的近似文本，时间久了字面会慢慢漂移。实现时把这句提成一个共享常量（放在 `agent24-domain::http`——`agent24-os-proto` 和 `agent24d::os_routes` 两边已经都依赖这个 crate，不需要新增依赖），六处的 `hint` 各自在这个常量前后拼自己的前缀/后缀，不是六处各自手打一遍。判据 11 相应改成断言这句**指令片段**在六处的 `hint` 里逐字出现，不是断言整条 `hint` 相等。

**v1 → v2（Codex High 7）**：v1 说「一百多处调用点」、把接线描述成三处生产调用（`domain.rs:1188`+`1233`、`os_routes.rs:816`、`server.rs:2021-2024`），两者都不对——`grep` 实测 `Current::new` 共 **64** 处，其中真正的生产接线只有 **`domain.rs:1188` 一处**（`os_routes.rs:816` 在 `#[cfg(test)] mod tests` 里，`server.rs:2024` 在 `#[cfg(test)] pub(crate) mod tests` 里，`domain.rs:1233` 不是构造点，是把同一个 `current` 传给 `proxy::mount()`）。这个错误的清单直接动摇了 v1 的机制选择：v1 提议 `Current::attach_status()` 是一个**可选**的、调用方自己决定要不要调的方法——真实生产接线只有一处不是「不该被迫多传参数」的问题，而是「唯一一处接线也可能漏调，编译器不会提醒」的问题。改成让**接线本身不可能被漏掉**：

- `Current::new` 签名仍然不变（64 处里的 63 处是纯 `Generation`/`drain` 语义测试，和 ERR-1 无关）。
- 真正的生产入口是 `supervisor::supervise()`（`supervisor.rs:405`）——它已经在函数体内创建了 `(status_tx, status_rx) = watch::channel(..)`（`supervisor.rs:416`），也已经拿到调用方传入的 `current: Arc<Current>` 参数（`supervisor.rs:408`，在 `Slot::claim(current)` 消费掉之前，先 `let current_for_status = current.clone();` 留一份）。让 `supervise()` 自己在创建 `status_rx` 之后立刻调用 `current_for_status.attach_status(status_rx.clone())`——这样**任何**调用 `supervise()` 的生产代码（今天只有 `domain.rs:1208` 一处，将来再加一处也一样）都自动拿到状态接线，不存在「记得调用可选方法」这一步。`domain.rs` 那段代码完全不用改。
- `attach_status()` 本身仍然是 `Current` 上的方法（供 `supervise()` 内部调用），但不再对外暴露成「调用方自己决定要不要用」的可选步骤。

- `refused_response` 改名或新增一个读了 `Current` 附带状态的版本（`proxy()` 里 `admit_request` 返回 `Err(RequestRefused::Stopping)` 的那一支，`proxy.rs:757`；以及 `(Abandoned{dispatched:false}, _) | (Ok(()), None)` 那一支，`proxy.rs:781`），把 `Stopping` 按最近一次 `Status` 的快照再细分：

  | `Status`（借用，不新增） | 新 `error.code` | `message`/`hint` 大意 |
  |---|---|---|
  | `Backoff{..}`（插槽此刻有占位活代，走既有 `NotReady` 分支，不经过本表） | 沿用 `module_not_ready` | 不变，D 段不碰 |
  | `Stopping`（这次运行正在被停止，重启决定还没做出——见下方 v3 说明） | 沿用 `module_stopping` | 「这次运行正在被停止」/「几秒内可能自动恢复（崩溃重启）也可能不会；等一下再 `agent24 os list` 看最终状态，或直接重试这次请求」 |
  | `GaveUp{failures, within, last}` | `circuit_breaker_tripped` | 「连续失败 {failures} 次（{within} 内），熔断器已跳，模块不会自动重启」/「运行 `agent24 os list` 看最近一次失败原因；确认修好后」+ **统一的重启句式**（见上方 v6 说明） |
  | `PackageChanged{reason}` | `package_changed` | `message` 见下方 v6 订正——不是照抄 `os_routes.rs:129-131`；`hint` 用**统一的重启句式**（见上方 v6 说明） |
  | `StopFailed{error}` | `stop_failed` | `message` 见下方 v6 订正——不是照抄 `os_routes.rs:592-596`；`hint` 用**统一的重启句式**（见上方 v6 说明） |
  | `Panicked` | `module_panicked`（新）| 「这次运行的监督者自己出错了（内核 bug）」/「看 daemon 日志找 panic 栈；模块本身可能没问题，问题在监督者」 |
  | `Killed` | `module_killed`（新）| 「daemon 关闭或监督循环被取消时，这次运行被直接杀掉，没有走正常停止」/「等 daemon 完全启动/停止后重试；如果反复出现，看 daemon 日志」 |
  | `Stopped` | 维持 `module_stopping` | 见下方 v3（Codex High 3）——不是「维持今天的措辞」，今天的措辞不够 |

  **v1 → v2（Codex High 6）**：v1 把 `Stopped`/`Panicked`/`Killed` 三者混在一起维持旧的 `module_stopping`，但 `os_routes.rs` 现有的状态渲染器已经把 `Panicked`/`Killed` 当成「degraded」单独描述，和「正常停止」区别对待——D 段的整个目的就是把插槽状态的既有区分透给代理请求的应答，三选一地放弃其中两种没有道理，改成三者各自的 code（`Panicked`/`Killed` 新增，`Stopped` 维持 `module_stopping`）。

  **v1 → v2（Codex High 5）**：v1 给 `GaveUp` 的 hint 写的是「`agent24 os enable` 手动恢复」——**实测是假的**。读了 `os_routes.rs` 的 `apply()`（约 438-479 行）：`enabled: true` 分支只把 `os.json` 里的开关写成 `true`，`handed` 恒为 `None`，不触发任何重新挂载/重新 `supervise()` 的动作；一个已经 `GaveUp` 的模块本来就处于「已启用但监督循环已经放弃」的状态，`agent24 os enable` 对它完全没有作用。这条 hint 如果照 v1 写出去，会把运维指向一个不会恢复任何东西的命令，比没有 hint 更糟。改成和 `PackageChanged`/`StopFailed` 一样的真实解法——重启 daemon（上表已更新）。**这是本轮最容易被忽视但后果最严重的一类错误——hint 的可信度本身是 ERR-1 的交付物，写错一条比不写更坏，写码时每一条 hint 都要能在真实代码路径上验证，不能从「听起来合理」推断。**

  **v2 → v3（Codex 第 2 轮 High 3）**：v2 给 `Stopped` 写的 hint 是「`agent24 os enable` 之后需要重新触发启动（具体触发方式……不能想当然）」——问题正是自己点名的那句「不能想当然」没有兑现，把待核实的问题原样留到了实现阶段，不是一个定稿的 hint。现在核实：`apply(enabled=true)` 只写配置、`handed` 恒 `None`（同 High 5 已经查过的那段代码），**并且** CLI 自己的文档说明启用要等到下次 daemon 启动才生效（`main.rs:77`）——所以对一个真正 `Stopped`（不是待自动重启）的模块，唯一真实有效的恢复路径和 `GaveUp`/`PackageChanged`/`StopFailed` 是同一条：`agent24 os enable`（如果它当前是被禁用停止的）**加上**重启 daemon（不是二选一，是两步都可能需要）。`hint` 改为：「这次运行已经正常停止，不会自己重启；如果它应该继续服务，先用 `agent24 os list` 确认它在 `os.json` 里是启用的（不是就 `agent24 os enable` 一下），然后」+ **统一的重启句式**（见上方 v6 说明）+「——只 `enable` 不重启不会让它重新跑起来」。

  **v3 → v4（Codex 第 3 轮 Medium 1，托管感知的重启命令）**：v3 给 `GaveUp`/`Stopped` 写的「重启 daemon」都直接铺开成字面的 `agent24 daemon stop && agent24 daemon start`——这句命令对一个用 `agent24 service install`（launchd）托管的 daemon 是不安全的：`stop` 让进程正常退出，launchd 的 `KeepAlive.SuccessfulExit=false` 认为「不需要拉起」；随后的 `start` 是手动 spawn，不走 `launchctl`，新进程从此脱离 launchd 监管，之后崩溃不会被自动拉起（`service.rs:96` 的 plist 配置，`followups.md` 已经记了这个坑，条目 **FU-68**，来源就是 FU-61 设计第 3 轮 Codex Medium）。FU-64/ERR-1 不重新解决 FU-68 本身（探测托管状态 + `launchctl kickstart` 是独立一块工作），只是不再无条件建议一条已知会导致脱管的命令。**v4 只改了 `GaveUp`/`Stopped` 两处，第 4 轮 Codex Medium 1 指出这不够**——见上方 v6 说明，改成六处全部统一（含计数订正）。

  **v5 → v6（Codex 第 5 轮 Medium 3，`PackageChanged`/`StopFailed` 的 `message` 不能照抄控制面文字）**：v5 让 `PackageChanged`/`StopFailed`（D 段，代理路径）的 `message` 去「抄 `os_routes.rs` 已有的状态说明」——查了实际文字才发现这行不通：`os_routes.rs:129-131`（`PackageChanged` 的 `os list` 文案）是一整句不可分割的字符串，状态说明和「重启 daemon 才能生效」那句不安全建议粘在一起，「只抄状态说明部分」没有给出切分的边界，实现时很容易把那句旧建议原样带进来，绕过判据 11（判据 11 只查 `hint`，`message` 里混进去的旧命令查不到）；`os_routes.rs:592-596`（`StopFailed` 的控制面文案，来自 `patch_os`）问题更大——那句话字面是「this request disabled {name} in os.json, but its supervisor could not stop it cleanly」，是针对**这次 PATCH 请求**说的话，套到一个被动打进这个模块命名空间的代理请求（用户根本没发过任何禁用请求）上是假话，而且可能带上 `write_error` 这种控制面才有意义的细节。**改法**：D 段这两行的 `message` 不再「抄」任何既有文字，改成直接从各自的 `Status` 载荷现挂现造、只描述代理请求视角下的事实：`PackageChanged{reason}` 的 `message` 是「the installed package changed or became unusable since this module was mounted: {reason}」（不带任何重启建议——建议全部在 `hint`）；`StopFailed{error}` 的 `message` 是「this module's last run could not be confirmed stopped: {error}」（同样不带重启建议，且完全不提「这次请求」/「os.json」这类只有控制面语境才成立的措辞）。`os_routes.rs` 自己端点的既有 `message`/`detail` 文字保持不动——那是另一个 endpoint 面对另一种请求（用户主动发的 PATCH/POST）该说的话，这次改动不碰它。判据相应加一条：新的代理路径 `message` 里不包含「os.json」「this request」这类只在控制面成立的措辞，且无条件的 `daemon stop && daemon start` 在整个应答（`message` 和 `hint` 都查）里都不出现，不只是查 `hint`。

  `Backoff` 已经核实过，不需要新 code：`supervisor.rs:637-645` 的既有注释和代码顺序证实——`policy.failed()` 返回 `Decision::RestartAfter(delay)` 时，**先** `slot.install(Generation::starting())` 装一个新的占位代，**再** `status.send_replace(Status::Backoff{..})` 广播状态；注释原话就是「直到下次握手完成前，插槽里是一个从没启动过的代：请求得到的是 `module_not_ready`（正在回来），不是已结束那次运行的 `module_stopping`」。也就是说**进入** `Backoff` 这一刻插槽从没出现过「没有任何代」的空当，`NotReady` 分支本来就对，D 段不用碰它、也不需要新增 `module_restarting`——上表 `Backoff` 那一行只是确认现状不变，不是本设计要交付的东西。

  **v1 → v2（Codex Medium 1，新增小节：进入终态时的竞态窗口）**：v1 只核实了「进入 `Backoff`」这一条转移的先后顺序，把结论错误地推广成「所有转移都没有空当」——不成立。`PackageChanged` 的既有代码（`supervisor.rs:679-680`）顺序反过来：**先** `slot.retire()`（撤销生成代，`Current::get()` 立刻能看到 `DrainState::Revoked`）**后** `status.send_replace(Status::PackageChanged{..})`，中间没有 `.await`，但在多线程 runtime 上这两行之间仍然存在另一个线程可以插进来观察的窗口——一个恰好在这个窗口里落地的代理请求，会看到「插槽已撤销」但 `Status` watch 还没更新到 `PackageChanged`（可能还读到上一个 `Running`/`Stopping`）。

  **v2 → v3（Codex 第 2 轮 High 2）**：v2 把这条窗口当成「短暂的读到过期值」来处理（退回通用 `module_stopping`）——低估了：核实 `finish()` 的真实顺序（`supervisor.rs:1099`、`1148`）发现 `Status::Stopping` **不是**一个转瞬即逝的中间态，而是监督者撤销生成代、广播 `Stopping`、然后**等待** `process.stop_then(..)`（可能长达整个 `stop_grace` 预算）之后才决定 `Backoff` 还是 `GaveUp` 的整段常规区间——插槽在这整段时间里都是「已撤销、状态是 `Stopping`」，这不是竞态例外，是**每一次停止都会经过的正常状态**，代理请求命中它的概率不低。v2 的表格没有 `Stopping` 这一行，会让这整段常规区间也退回旧的、没有 hint 的通用 `module_stopping`，还给它配了一句「正常停止、不会再回来」的确定性 hint（v2 为 `Stopped`写的）——对一个可能几秒后就自动恢复的模块来说这是一句误导。改法：**给 `Stopping` 单独一行**（上表已加），措辞刻意保持不确定（「可能自动恢复也可能不会」），不假装知道最终结果；`Status` 真正落到 `GaveUp`/`Stopped` 等终态之后，下一个请求自然会读到细分之后的准确分类。「尽力而为，读不到手表格的 `Status` 就退回旧行为」这条兜底逻辑保留，但现在它真正覆盖的只是**极窄的实现细节窗口**（比如 `retire()` 和 `send_replace()` 之间那两行没有 `.await` 的间隙，或者未来 `Status` 新增变体还没来得及补进表格）——不再是拿它去掩盖一整段常规状态。判据 9 相应地把测试重点换成真实的 `Status::Stopping`（构造一个撤销了的 `Current` 配 `Status::Stopping`，断言命中新的那一行，不是退回 `module_stopping`），窄义的「表格之外的未知值退回旧行为」单独留一条断言（**v3 → v4，Codex 第 3 轮 High 2**：夹具怎么构造、以及退回路径本身也要带非空 `hint`，见判据 9 的最新文字——这条兜底既然报的还是 `module_stopping`，就要遵守 E 段「每个 `module_stopping` 应答都要有非空 hint」的同一条规则，不能因为是兜底分支就悄悄豁免）。

### E. ERR-1 的完整覆盖面（Codex 第 2 轮 High 1，新增小节）

**v2 的缺口**：`tasks.md:111` 原文点名的是七个码——`module_not_ready` / `module_draining` / `module_stopping` / 熔断 / `package_changed` / `disable_pending` / `stop_failed`——v2 的 D 段只覆盖了「插槽没有活代」这一支下面的几个终态，漏了 `NotReady`/`Draining`（今天走 `refused_response`，`proxy.rs:801-812`，有说明性的 `message` 但没有结构化 `hint`）和两个纯控制面的响应（`disable_pending`/`stop_failed`，来自 `PATCH /api/v1/os/{name}` 与 `POST /api/v1/os/{name}/stop`，`os_routes.rs:592/615/664/669`，同样有说明性文字但混在 `message` 里，没有独立的 `hint` 字段）。这些call site 不需要新的分类机制（不像 D 段要读 `Status` 才能细分），只需要把既有的「该做什么」那半句话搬进新的 `hint` 字段——纯粹是补面，不是新设计。逐一列出，写码时照此清单核对，不能少一个：

| `error.code` | 来源 | `message`（不变） | `hint`（新增） |
|---|---|---|---|
| `module_not_ready` | `refused_response(RequestRefused::NotReady)`，`proxy.rs:803` | 不变 | 「等一下再重试；如果这个模块反复卡在这个状态，`agent24 os list` 看是不是在崩溃重启」 |
| `module_draining` | `refused_response(RequestRefused::Draining)`，`proxy.rs:804-807` | 不变 | 「这个生成代只处理已经收下的请求，不再收新请求；`agent24 os list` 显示模块重新 running 之后再重试」（**v3 → v4，Codex 第 3 轮 High 1**：原文「下一个生成代起来后请求会正常处理」是假的——`Draining` 今天的唯一产生路径是显式停止请求（`stop_requested` 赢了之后调 `begin_drain()`，随后 `Run::StopRequested` 终止，不开下一代，`supervisor.rs:936/1056`），热禁用是最常见的例子，压根不会有「下一个生成代」。改成不承诺自动恢复的措辞，让读者自己去 `os list` 确认，而不是替它打包票） |
| `module_stopping` / `circuit_breaker_tripped` / `package_changed` / `stop_failed` / `module_panicked` / `module_killed` | D 段的 `Status` 细分（含新增的 `Stopping` 行） | 见 D 段表格 | 见 D 段表格 |
| `disable_pending` | `patch_os`（`os_routes.rs:615`）与 `stop_now_os`（`os_routes.rs:669`） | 不变 | 「过一会儿再 `agent24 os list` 看它是不是已经停了；这次请求可以重试」 |
| `stop_failed`（控制面版本，与 D 段的 `StopFailed` 是同一个 code、不同的调用点，共两个调用点：`patch_os`/`stop_now_os`） | `patch_os`（`os_routes.rs:592`）与 `stop_now_os`（`os_routes.rs:664`） | 不变——这两处是控制面自己的既有措辞，符合各自的语境（用户确实刚发过一次 PATCH/POST），不像 D 段那两行需要改写，见 D 段 v6 的 Medium 3 说明 | 「`agent24 os list` 看具体原因；」+ **统一的重启句式**（D 段 v6 说明——**v4 → v5，Codex 第 4 轮 Medium 1；六处计数订正见 v6**：这两处 v4 原本只写「需要重启 daemon 才能恢复」，和 `GaveUp`/`Stopped` 用的托管感知说法不一致，也一样有 launchd 脱管风险，改用同一条标准句式） |

**范围收窄，不是遗漏**：`upstream_unavailable`（FU-64 C 段维持的「模块确定不可达」502）**不在**上面这七个码里——`tasks.md:111` 原文没有点它的名字，FU-64 已经单独给了它一个结构化的姊妹 code（`upstream_connection_closed`，用在「可能已送达」那半），而「确定不可达」这半今天的 `message`（比如「the module could not be reached: {e}」）本身已经说明了状况，只是没有「下一步」。这次不给它补 `hint` 是刻意的范围收窄，不是看漏了——原因是它的失败原因太杂（模块目录不存在、进程没监听、socket 权限问题……），需要先分类失败原因才能给出靠谱的「下一步」，这本身是另一块设计（很可能和 FU-57 的 `FailureKind` 思路相关），塞进这次的范围会让本设计膨胀。留作独立 followup。

**判据措辞收紧（回应「折进 message 也算」的漏洞）**：判据 5 原稿允许 hint「折进 `message` 的等价文本」也算过——这弱化了 ERR-1 的交付物本身（结构化的 `error.hint` 字段），已在下面判据部分改正：每一条都必须断言 `error.hint` 是非空字符串，不接受「文字出现在 message 里就算」。

### `ErrorBody` 协议改动

`agent24-protocol/src/types.rs:176` 加一个字段：

```rust
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}
```

选显式具名字段而不是塞进既有的 `details: Map`：`details` 从落地到今天没有一次真正用过，这次是第一次有内容要放进错误应答，选它意味着两件不同的事（“结构化附加数据”和“下一步该做什么”）共用一个无类型容器，往后每次要不要往 `details` 里塞东西都要先想一遍是不是在偷渡 hint 的语义。`hint` 单独成一个 `Option<String>`，和 `message`（是什么状况）并列、职责分开，`JsonSchema` 派生不受影响（`Option<String>` 和现有的 `Option<Map<..>>`同构）。**Codex 第 1 轮已确认这个选择站得住**（无异议，见文末「无发现」项），不再是开放问题。

`error_response()`（`agent24-domain/src/http.rs:35`）签名从 `(status, code, message)` 加一个可选重载或者新函数 `error_response_with_hint(status, code, message, hint)`——不改所有既有调用点的签名（FU-57/FU-60/FU-61 时代的 `error_response(` 调用有几十处，几乎都不需要 hint，被迫每处补一个 `None` 是纯噪音）。

**v1 → v2（Codex Medium 6，新增小节：协议改动的完整面）**：`ErrorBody` 加字段不是只改这一个 Rust struct 就完工——这个仓库把协议当契约认真对待（CI 里就有 `Contract Tests + Codegen Drift` 这一档），本设计的实现阶段需要同步：checked-in 的 OpenAPI schema、`SPEC-002-protocol.md`（协议文档里 `ErrorBody` 的字段说明）、生成的 TypeScript 客户端（`openapi.d.ts` 一类）、生成的 events schema（如果它也引用了 `ErrorBody`）、以及代码里手写 `ErrorBody{..}` 字面量的地方（补上 `hint: None`）。这些不是新的设计决策，是既有 codegen 流程本来就会覆盖的例行步骤，写进这里是为了不让实现阶段漏掉——`Contract Tests + Codegen Drift` 这一档 CI 本来就会在漏改时红，但赶在 PR 里才发现比设计阶段列清楚要贵。

### CLI

**v1 → v2（Codex Medium 5）**：v1 说「三处打印 `error.message` 的调用点（533 / 604 / 391）」——实测不对。真正读取 `error.message` 字段的只有两处：`main.rs:534`（热禁用尽力而为提示）和 `main.rs:606`（`agent24 os` 系列命令）；第 391 行附近是 `Err(format!("daemon returned {}", res.status()))`，压根没解析响应体；`cmd_chat`（约 352-355 行）遇到非 2xx 直接把原始 body 转发出去，不走这条解析路径。清单错了，但 v1「不新建共享 helper」的结论也要跟着改——**两个**真实调用点解析的是同一个 JSON 形状（`body["error"]["code"|"message"|"hint"]`），共享一次解析现在确实划算，不是过度设计：加一个小函数（比如 `fn daemon_error(body: &Value) -> Option<(String, Option<String>)>` 或等价形状，返回 `message` 和可选的 `hint`），`main.rs:534`/`606` 都改用它；有 `hint` 就拼在 `message` 后面另起一行打印（原样，不加 CLI 自己的措辞包装——"CLI 原样打印" 判据的字面意思），没有就维持今天的单行。`cmd_chat` 保持透传原始 body，不纳入这次改动——它转发的不是「daemon 的控制面错误」，是模块自己的应答。

## 判据（每条带正对照）

1. **FU-64 主判据（压力覆盖，不是变异的正对照——见判据 7）**：一个每次应答后立即（无延迟）关闭连接的合成模块，连续 2000 个无间隔 GET 请求，零 502。**v5 → v6（Codex 第 5 轮 Low）**：这条本身不保证抓到变异——hyper 可能一直在入池前就看到关闭，2000 次全程走的都是全新连接，去掉 A 段重试的变异照样能全程零 502、测试假绿。这条只作为大样本的压力覆盖保留，真正杀变异的断言在判据 7 的确定性夹具里。
2. **FU-64 POST 判据（同上，压力覆盖）**：同一合成模块，POST（带非空 body）序列请求，凡是撞进窗口 (b) 的都得到 `502`，`error.code == "upstream_connection_closed"`，`error.hint` 非空且字面包含判据文本里那两句话（「确认无副作用后重试」「调大…keep-alive」）。**v5 → v6（Codex 第 5 轮 Low）**：和判据 1 同样的问题——这 2000 次里到底有没有真的撞进窗口 (b) 不是确定的，这条断言的是「撞进去的那些长什么样」，不是「一定会撞进去」；真正保证一定撞进窗口 (b) 并杀掉 C 段分叉去掉这个变异的，是判据 7。
3. **窗口 (a) 的确定性测试**（补 FU-63 遗留的空档）：一个仅测试可见的钩子，在 `take()` 和 `try_send_request()` 之间的确定时点关闭连接（不靠计时/轮询猜时机）；两个连续无请求体请求都拿到 200。正对照：去掉「取回未发出的请求并重发」那支分支（`e.take_message()` 的 `Some` 分支直接当失败处理）——第二个请求变 502。
4. **B 段判据**：一条连接 `put()` 入池后，经过 `IDLE_MAX_AGE` 以上（测试用可注入的时钟或缩短的常量，不真的睡 4 秒），下一次 `take()` 对它不可见（即使连接本身还活着、`is_closed()` 为 `false`）。正对照：把年龄检查去掉——同样陈旧的连接仍被 `take()` 选中。
5. **ERR-1 判据（D 段）**：`circuit_breaker_tripped` / `package_changed` / `stop_failed` / `module_panicked` / `module_killed` / `module_stopping`（`Stopped` 与新增的 `Stopping` 两种取值各一条，不能只测一种）每个错误码各一条测试——构造对应的 `Status`，打一个请求到该插槽，断言 `error.code` 命中、**`error.hint` 字段本身是非空字符串**（v2→v3：不再接受「折进 `message`」，见 E 段收尾说明）且写明一个可执行的下一步（不是「出错了」这种空话——例如断言字符串里包含一个具体命令名或具体动作动词，且这个命令/动作在真实代码路径上确实能做到它说的事——`GaveUp` 那条测试尤其要断言 hint **不**包含 `agent24 os enable`，防止 High 5 那种错误静默复发；`Stopped` 那条要断言 hint 同时提到 `enable` 和重启 daemon 两步，防止 High 3 那种「只说一半」复发）。正对照：把 D 段的 `Status` 细分去掉、直接退回统一的 `module_stopping`——断言 `code` 不匹配从而变红（**v3 → v4，Codex 第 3 轮 Low 1**：这句正对照对 `GaveUp`/`PackageChanged`/`StopFailed`/`Panicked`/`Killed` 五条成立，但 `Stopping`/`Stopped` 两条本来就刻意共用 `module_stopping`，去掉细分之后 `code` 不会变——这两条的正对照改成断言 `message`/`hint` 的具体文字：去掉细分后二者都会退回今天单一的通用文案，用「hint 不再区分『可能自动恢复』还是『已确定不会』这两种语义」来断言变异被抓住，不是靠 `code` 不匹配）。
6. **回归**：FU-61 判据 6 提到的「`PackageChanged` 状态下代理请求仍是旧的 `module_stopping`」这条已知缺口，本设计的 D 段应该让它变绿——写一条测试明确断言（不只是新增能力，还要断言旧缺口确实被关上）。
7. **窗口 (b) 的确定性夹具（Codex 第 1 轮 High 4，新增；第 5 轮 Low 确认这条才是真正的正对照，判据 1/2 只是压力覆盖）**：判据 1/2 的「2000 次」本身不保证真的每次都撞进窗口 (b)——hyper 可能在入池前就看到关闭，或者 `take()` 的存活过滤先一步拦截，导致 2000 次全部干净地走了全新连接，去掉 A 段重试或去掉 C 段分叉的变异都未必真的会让判据 1/2 变红（假绿）。这条确定性夹具才是真正能杀变异的正对照：预热一条连接、确保它就是即将被复用的那条，在**读到重试请求的完整字节**之后再关闭（不预先关闭——逼真实的「已经交给 hyper」时刻，不是「入池前」），断言：① 幂等 GET 场景下第二次物理尝试确实发生、客户端拿到 200（正对照：去掉 A 段重试——这条会稳定变红，不再依赖运气）；② 非幂等 POST 场景下原始请求只被这条 fixture 观察到**一次**（不会被误发两次），客户端拿到判据 2 要求的结构化 502（正对照：去掉 C 段的分叉——这条同样稳定变红）。2000 次循环保留作为额外的压力/统计覆盖，但不再是唯一证据，也不再单独承担正对照的职责。
8. **重试前的撤销检查（Codex 第 1 轮 High 3；断言内容按第 2 轮 Medium 1 订正）**：构造一个请求，在 `exchange()` 第一次返回 `MaybeSent` 之后、A 段发起重试之前的确定时点（测试用钩子，不靠计时）撤销该请求的生成代；断言不发生第二次物理发送（synthetic 模块只观察到一次连接尝试），且客户端应答的 `error.code` 精确等于 `"request_abandoned"`（不是判据 1/2 期待的 200，也不是 C 段的 `upstream_connection_closed`——`in_flight.finish()` 在生成代已撤销时返回 `Abandoned{dispatched:true}` 是权威判定，`proxy()` 会丢弃 `forward()` 算出的任何结果，见 A 段 v3 说明）。正对照：去掉重试前的撤销复查——断言变成「合成模块观察到了第二次连接尝试」。
9. **D 段的竞态回退（Codex 第 1 轮 Medium 1；范围按第 2 轮 High 2 收窄；夹具按第 3 轮 High 2 订正）**：两条测试，覆盖两件不同的事——① 真实的 `Status::Stopping`：构造一个生成代已撤销、`Status` 确实是 `Stopping` 的 `Current`，断言命中 D 段表格里新增的 `Stopping` 行（`error.code == "module_stopping"`，`hint` 措辞不确定、不承诺自动恢复也不承诺不恢复），这是覆盖「每次停止都会经过」的常规区间，不是边缘情形；② 窄义的兜底：**v3 → v4（Codex 第 3 轮 High 2）**：v3 说用「表格完全没覆盖的值（人工构造，比如未来才会有的假想变体）」做夹具——这条测不出来：`Status` 是个真实存在的 Rust enum，测试代码构造不出一个「表格没覆盖的值」而不去真的新增一个变体，这和「不新增状态机」的既定约束直接矛盾。改用一个**真实存在**的转瞬窗口做夹具（三选一，任选其一即可，不用全测）：`PackageChanged` 落地前生成代已撤销但 `Status` 接收端仍可能读到上一个 `Backoff`（`supervisor.rs:677`）；`Exit::drop` 在发布 `Panicked`/`Killed` 之前先撤销生成代，接收端仍可能读到 `Starting`/`Running`（`supervisor.rs:554`）；或者 `finish()` 撤销之后、正式发布 `Stopping` 之前的一瞬间，接收端仍是撤销前的上一个状态（`supervisor.rs:1099`）——挑其中最好搭的一个，手动构造出「生成代已撤销 + `Status` 还没跟上」这个真实存在但转瞬即逝的组合（不用真的赢下一场线程竞速，直接手动摆出这个状态组合即可），断言应答退回今天的 `module_stopping`、**且 `hint` 非空**（v3 只断言了 code 和不 panic，这条第 3 轮 High 2 指出：ERR-1 说每个 `module_stopping` 应答都要有非空 hint，这条兜底路径也不例外——给它配一句中性、不误导的话，比如「这个模块的状态刚发生变化，`agent24 os list` 能看到最新情况」）、不 panic。
10. **E 段判据（Codex 第 2 轮 High 1）**：`module_not_ready`、`module_draining`、控制面的两个 `disable_pending`（`patch_os`/`stop_now_os` 各一条）、控制面的 `stop_failed`（同样两处各一条）——每处一条测试，断言 `error.hint` 非空字符串，`message` 保持今天的原文不变（这次只是把「下一步」从 `message` 里搬出来一份到 `hint`，不是重写状态说明）。
11. **统一重启句式判据（Codex 第 4 轮 Medium 1，新增；计数与断言内容按第 5 轮订正）**：对全部**六处**用到「重启 daemon」的响应——`circuit_breaker_tripped`（`GaveUp`）、`module_stopping`（`Stopped` 这一支）、`package_changed`、D 段的 `stop_failed`、E 段 `patch_os` 的 `stop_failed`、E 段 `stop_now_os` 的 `stop_failed`（共六个调用点，不是四轮 Codex 审查时误数的五个）——各一条测试，断言：① 六处的 `hint` 里都逐字出现同一段**重启指令片段**（D 段 v6 说明里那句加引号的完整指令：「先 `agent24 service status`：`loaded: yes` 就用 `launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24`；`loaded: no` 才用 `agent24 daemon stop && agent24 daemon start`」）——断言的是这个共享片段逐字相同，不是断言六条完整 `hint` 相等（`Stopped`/`GaveUp` 等各自在片段前后还有自己专属的前缀/后缀，不该被误判为不一致）；② 这段片段本身包含完整可执行的 `launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24`（带真实的 service target，不是占位符），不接受只判定「出现了 `launchctl kickstart` 这几个字」就算过；③ 六处的**整个应答**（`message` 和 `hint` 都要查，不只是 `hint`）里，`agent24 daemon stop && agent24 daemon start` 这句命令不会不带条件地单独出现——必须伴随判断托管状态的那半句话，不能被拆开、剩裸命令流出去；④ `PackageChanged`/`StopFailed`（D 段）这两条另外断言 `message` 里不包含「os.json」「this request」这类只在控制面语境成立的措辞（呼应上面 v6 的 Medium 3 修复）。任何一处漏掉、走样，或者六处的共享片段彼此不是逐字相同，测试直接失败——这条断言本身就是防止「改了两处、漏了三处」（第 4 轮 Medium 1 的原始问题）和「命令带占位符跑不了」（第 5 轮 Medium 2）复发的正对照。

## 不改的东西

- `Generation`/`DrainState` 状态机（`drain.rs` 的三态图）——不加状态，不改迁移。
- `supervisor::Status` 的任何变体或迁移——只读，不写。
- 窗口 (a) 的处理逻辑本身（FU-63/PR#184 已经做对）——只补测试。
- 非幂等/带 body 请求的行为——明确不重试，落 C 段。
- `error_response()` 的既有调用点签名——加一个新函数/重载，不改旧的。
- `Current::new` 的签名——生产接线改在 `supervise()` 内部完成（见 D 段 Codex High 7）。

## v1 → v2 改动（Codex 第 1 轮：0 Critical、7 High、6 Medium、0 Low，全部采纳）

- **High 1**：`ExchangeError` 改按送达确定性（`NotSent`/`MaybeSent`）分类，不按连接是新是旧；C 段的最终判定改成「出现过一次 `MaybeSent` 就不可逆」，不是只看最后一次尝试。
- **High 2**：A 段的显式重试改成强制跳过连接池、必须用全新连接，不再允许重试又抓到另一条陈旧的池中连接。
- **High 3**：A 段的重试发送之前，新增一次撤销复查（`dispatch()` 或等价只读检查）——今天只发生一次物理发送、这个检查从未缺过；FU-64 引入第二次物理发送之后，这是本设计真正新增的竞态，必须显式处理。
- **High 4**：判据 1/2 从纯统计性的「2000 次循环」改为「确定性夹具 + 统计覆盖」两层——新增判据 7。
- **High 5**：D 段 `GaveUp` 的 hint 从「`agent24 os enable` 手动恢复」（读代码验证后确认是假的）改成和 `PackageChanged`/`StopFailed` 一致的「重启 daemon」。
- **High 6**：D 段不再把 `Stopped`/`Panicked`/`Killed` 三态合并成一个 `module_stopping`——`Panicked`/`Killed` 各自新增 code，`Stopped` 维持但补上非空 hint。
- **High 7**：`Current::new` 调用点数从「一百多处」订正为 64 处（仅 `domain.rs:1188` 是生产构造），机制从「调用方可选调用 `attach_status()`」改成「`supervise()` 内部自动接线」，消除「唯一生产接线点也可能被漏掉」的风险。
- **Medium 1**（新增小节）：核实「进入终态时插槽是否存在空当」不能只查 `Backoff` 一条转移就类推——`PackageChanged` 的落地顺序（先撤销后广播状态）确实存在跨线程可观察的窗口。对策不是同步原语，是把 D 段的分类定性为「尽力而为」，读到不认识的 `Status` 一律回退今天的 `module_stopping`；新增判据 9 覆盖这条回退路径本身。
- **Medium 2**（新增小节）：显式写出 A 段重试的信任边界——建立在 RFC 9110 §9.2.2「安全方法」之上，不合规模块的副作用是既有问题的暴露，不是本设计引入的新问题；模块级选择退出开关是独立 followup。
- **Medium 3**：`IDLE_MAX_AGE=4000ms` 的措辞从「需要一个……默认值」改成明确的「启发式、尽力而为，不是正确性边界」；per-module 配置延后为 followup。
- **Medium 4**（新增小节）：新增的 `A24_MODULE_IDLE_CONN_MAX_MS` 必须同时进 `agent24-cli/src/service.rs` 的 `PASSTHROUGH_VARS` 白名单，否则 launchd 安装的 daemon 读不到——照抄 SHUT-1b 的先例。
- **Medium 5**：CLI 小节的调用点清单从「三处 533/604/391」订正为「两处 534/606，391 不解析响应体」；订正后两个真实调用点共享同一 JSON 形状，新增一个小的共享解析函数（v1 认为不值得新建 helper 的结论被推翻）。
- **Medium 6**（新增小节）：列出 `ErrorBody` 加字段需要同步的协议制品（OpenAPI schema、`SPEC-002-protocol.md`、生成的 TS 客户端、事件 schema、手写字面量），对齐仓库已有的 `Contract Tests + Codegen Drift` CI 档位。
- （Low：本轮无。）
- **无发现（Codex 主动核实过、认为站得住，原样保留）**：`hint: Option<String>` 优于复用 `details: Map`；在重试时复用同一个已 admit 的 `Generation`（而不是重新读一次 `Current::get()`）；`Backoff` 进入时刻插槽不留空当（真正的缺口是 Medium 1 指出的*退出*到终态时的窗口，不是这条本身）。

## v2 → v3 改动（Codex 第 2 轮：0 Critical、3 High、1 Medium、0 Low，全部采纳）

- **High 1**：ERR-1 覆盖面订正——v2 的 D 段只顾上了「插槽没有活代」的那几个终态，漏了 `module_not_ready`/`module_draining`（今天有说明性 `message` 但没有独立 `hint`）和两个纯控制面响应 `disable_pending`/`stop_failed`（`os_routes.rs` 的 `patch_os`/`stop_now_os`）。新增 E 段把 `tasks.md:111` 点名的七个码逐一列清楚，`upstream_unavailable` 显式记为刻意收窄（原文没点它的名字），判据 5/10 改成要求 `error.hint` 真的是独立字段，不再接受「折进 message」。
- **High 2**：D 段的「尽力而为回退」原来把 `Status::Stopping` 当成短暂竞态处理——核实 `finish()` 的真实时序（撤销 → 广播 `Stopping` → 等 `stop_then` 可能等到整个 `stop_grace`）之后发现这是每次停止都会经过的常规区间，不是例外。表格新增 `Stopping` 专属一行（措辞刻意不确定，不假装知道会不会自动恢复），「读到陌生值退回旧行为」的兜底收窄回它原本该覆盖的窄范围（真正未知/未来的 `Status` 变体）。判据 9 拆成两条，分别测真实的 `Stopping` 和窄义的兜底。
- **High 3**：`Stopped` 的 hint 在 v2 里留了一句「不能想当然」但自己没兑现（把触发方式的核实留到写码阶段）。现在核实完：`enable` 单独不会重启，CLI 自己的文档也说启用要等下次 daemon 启动生效——改成「先确认启用状态，再重启 daemon」两步都写全的最终文本，不再留尾巴。判据 5 补一条断言 `Stopped` 的 hint 同时点到这两步。
- **Medium 1**：A 段「重试前撤销检查失败」那句话原来说要把结果按 `MaybeSent` 交给 C 段——和判据 8 的预期矛盾。核实 `proxy()` 的外层逻辑（`in_flight.finish()` 返回 `Abandoned{dispatched:true}` 时无条件丢弃 `forward()` 的结果，改报 `request_abandoned`）之后确认：这条路径的客户端应答永远是 `request_abandoned`，不会是 C 段的 502，这是 `finish()` 的既有逻辑保证的，不需要新代码，只是 v2 的文字说错了优先级。判据 8 改成断言精确的 `request_abandoned` code。
- （Low：本轮无。）

## v3 → v4 改动（Codex 第 3 轮：0 Critical、2 High、1 Medium、1 Low，全部采纳）

- **High 1**：`module_draining` 的 hint 写的是「下一个生成代起来后请求会正常处理」——核实 `Draining` 唯一的产生路径（显式停止请求，`supervisor.rs:936/1056`）之后发现这是假的：热禁用（最常见的例子）压根不会有下一代。改成不承诺自动恢复的措辞，指路到 `agent24 os list` 让读者自己确认，不替它打包票。
- **High 2**：判据 9②「用表格没覆盖的值做夹具」测不出来——`Status` 是真实 enum，构造一个「没覆盖的值」等于新增变体，和「不新增状态机」的约束矛盾。改用三个真实存在的转瞬窗口之一（`PackageChanged`/`Panicked`/`Killed` 落地前、或 `finish()` 发布 `Stopping` 前）手动摆出「生成代已撤销 + `Status` 还没跟上」的组合，不用真的赢线程竞速。同时补上这条兜底本身也要断言非空 `hint`——它报的还是 `module_stopping`，不能因为是兜底分支就豁免 E 段「每个 `module_stopping` 都要有 hint」的规则。
- **Medium 1**：`GaveUp`/`Stopped` 的 hint 里字面写死的 `agent24 daemon stop && agent24 daemon start` 对 launchd 托管的 daemon 不安全（会导致脱管，之后崩溃不再自动拉起）——这个坑仓库里已经有跟踪条目 `followups.md` **FU-68**，来源正是 FU-61 设计第 3 轮 Codex 挑出同一句话（`package_changed`/`stop_failed` 两条沿用的正是这句带缺陷的既定说法）。FU-64/ERR-1 不重新解决 FU-68，只是不再无条件地把这条命令写死——改成先看是否被服务托管、再挑对应的重启方式，FU-68 落地后连同 `PackageChanged`/`StopFailed` 一起换成它提供的统一安全命令。
- **Low 1**：判据 5 的正对照对 `Stopping`/`Stopped` 两行不成立——这两行刻意共用 `module_stopping`，去掉状态细分不会让 `code` 变化。改成断言这两行的 `message`/`hint` 文字本身被抹平（不再区分「可能恢复」和「确定不恢复」两种语义），不是断言 `code` 不匹配。

## v4 → v5 改动（Codex 第 4 轮：0 Critical、0 High、1 Medium、0 Low，全部采纳；三处 v3 修复复核为正确，独立通读全文未发现新问题）

- **Medium 1**：v4 的托管感知重启措辞只改了 `GaveUp`/`Stopped` 两处，`PackageChanged`（D 段）和 `StopFailed`（D 段一处 + E 段控制面两处）一共三个地方仍然是「抄旧文字」或「裸的『需要重启 daemon』」，同一个已知风险留了三处没堵。改法：定一条**唯一的**标准句式（`agent24 service status` 判断托管 → 托管用 `launchctl kickstart`、没托管才用 `daemon stop && daemon start`），五处全部照抄这一句，不再各写各的；`PackageChanged` 不再原样搬 `os_routes.rs` 的旧文字（那份旧文字本身就是问题所在，`os_routes.rs` 自己的既有 `detail` 字段不动，只是这次新增的 `hint` 字段不抄它）。新增判据 11，一次性断言五处的 `hint` 都包含 `agent24 service status`、都不是无条件的 `daemon stop && daemon start`、都提到 `launchctl kickstart`，且五处文字彼此一致——直接把「改了两处漏三处」这类问题变成会变红的测试，不再靠人工核对清单。

## v5 → v6 改动（Codex 第 5 轮：0 Critical、0 High、3 Medium、2 Low，全部采纳）

- **Medium 1**：v5 说「六处」建议重启但反复写成「五处」（本节标题、判据 11 都数错），把 D 段的 `StopFailed` 和 E 段两个控制面 `stop_failed` 算重了。订正为六处，逐处点名。同时 v5 要求六处「完全一致」不成立——`Stopped`/`GaveUp` 等各自有专属前后缀，真正要保证一致的是共享的那句重启指令片段，不是整条 `hint`。改成：只有这句指令片段本身要求逐字相同、来自同一个共享 Rust 常量（放 `agent24-domain::http`），六处各自在片段前后拼自己的话；判据 11 相应改成断言片段一致，不是断言整条 `hint` 相等。
- **Medium 2**：`launchctl kickstart -k <label>` 里的 `<label>` 是没法照抄执行的占位符。核实 `service.rs` 的 `LABEL` 常量和 target 组装方式、以及 `agent24 service status` 实际打印的字段（`loaded: yes/no`）之后，标准句式改成给出真实可执行的完整命令 `launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24`，判据 11 相应要求断言的是这条完整命令，不是子串 `launchctl kickstart`。
- **Medium 3**：D 段 `PackageChanged`/`StopFailed` 两行原本让 `message` 去「抄」`os_routes.rs` 的既有文字——查了原文才发现行不通：`PackageChanged` 那句是一个不可分割的字符串，状态说明和不安全的重启建议粘在一起，没给出切分边界；`StopFailed`（控制面）那句字面说「this request disabled {name} in os.json」，套到一个被动的代理请求上是假话。改成这两行的 `message` 不再抄任何既有文字，直接从 `Status` 载荷现造只描述代理视角事实的句子，不带任何重启建议（建议全部在 `hint`）；E 段控制面的两个 `stop_failed` 的 `message` 维持不变——那是控制面自己该说的话，语境成立，不用改。判据 11 相应加断言：`message` 不出现「os.json」「this request」这类只在控制面成立的措辞，且无条件命令在整个应答（不只是 `hint`）里都不出现。
- **Low 1**：B 段开头一句「`IDLE_MAX_AGE` 需要一个……默认值」和紧接着 v1→v2 订正段落说的「不再说『需要一个……默认值』」自相矛盾——是订正时只改了后面的解释段、忘了回头改开头那句原文。顺手改成和订正后的语气一致的措辞。
- **Low 2**：判据 1/2 的「正对照」原文暗示「去掉 A/C 段的改动，2000 次里一定会变红」——判据 7 自己已经指出这不成立（2000 次可能全程没撞进窗口 (b)，变异照样测不出来）。改成判据 1/2 明确降级为压力覆盖，判据 7 才是真正杀变异的正对照——两条判据的措辞现在互相印证，不再有一条隐含着另一条已经证伪的断言。

至此本设计已经过 5 轮 Codex 挑战（0 Critical / 12 High / 12 Medium / 3 Low，累计全部采纳）。第 5 轮明确说「只剩这几条 Medium/Low，没发现 Critical/High，独立通读没找到新问题」——这轮修复偏收尾性质（计数订正、命令可执行性、文本一致性），不是新发现的设计缺陷。

## 第 6 轮（终审）：clean, ready to code

逐一复核 v6 的 5 处修复（六处计数、可执行的 `launchctl` 命令、`agent24-domain::http` 作为共享常量的落点合理、`PackageChanged`/`StopFailed` 的新 `message` 不含任何控制面专属措辞、判据 11 断言的是共享片段而非整条相等）全部正确完整；对全文 A–E 各段、协议改动、CLI 改动、全部 11 条判据、范围收窄说明、历轮改动记录做了一次独立通读，**没有发现新问题**。

**0 Critical / 0 High / 0 Medium / 0 Low。判定：clean, ready to code。**

累计 6 轮：0 Critical / 12 High / 12 Medium / 3 Low，全部采纳。设计阶段到此结束，可以开始写代码。
