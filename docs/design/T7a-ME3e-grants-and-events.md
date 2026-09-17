# T7a / ME-3e（拆分 1/2）—— 进程外模块的能力授予接线 + 事件回调

## 与 T7b 的分工

T7/ME-3e 原计划一次性交付「事件 + 审批」。设计过程中（Codex 第 1 轮评审，0 Critical / 7 High / 7 Medium / 4 Low）发现两件事：

1. 审批（gate/advise）需要的东西比最初设计文档写的多得多：一次性令牌要在同一临界区内和状态检查一起做完（否则有 TOCTOU）、批准要在“用户想了很久”这段等待期结束时重新核实原请求是否还活着（不是只在回调一开始查一次）、需要一个进程内 `ApprovalRequester` 句柄跟 `Capability::Approval` 对应（现有 `Capability::Events` ↔ `KernelCtx::events()` 是先例）、需要一个真正能表达 pending/timeout/aborted 的状态机、需要一个人能实际看到并操作的客户端（不能只加一组没人读的 REST + WS 事件）。这些都是状态机/安全敏感设计，配得上单独一轮评审，硬塞进同一份文档只会让评审信噪比变差。
2. 但审批和事件**共享同一个更底层的缺口**：进程外模块的能力授予（capability grant）今天**完全没有接线**——生产环境握手写死 `Offer::none()`（`agent24-os-proto/src/supervisor.rs:851`），`MountReport.granted` 对所有进程外模块永远是空集合（`agent24d/src/domain.rs:1121-1131`，代码里的注释原话：「No capability is granted to an out-of-process module yet: the handshake offers none (`Offer::none`), so it holds none」）。事件和审批都得先有这条线，才能谈「模块声明了 events/approval 能力、内核按声明真授予、未授予的调用被明确拒绝」。（**用词订正，Codex 第 3 轮 Medium 1**：不是「未授予的方法根本不存在」——那是上一版的说法，v2 已经改成「方法始终注册，未授予在调用时返回 `forbidden`」，本节这句话跟着一起改，避免这里和 §3 自相矛盾。）

**所以拆成两轮**：T7a（本文档）先把「进程外模块能不能拿到能力授予」这条地基修好，并用**事件回调**这个相对简单、无状态、不涉及用户决策等待的功能把地基跑通、测出来。T7b 在 T7a 合并之后另开文档，专门做审批（gate/advise），直接复用 T7a 交付的授予机制，不用重新设计一遍。

T7a 范围明确不包含：`approval_token`、`ModuleApproval`、`_a24/approval/gate`、`_a24/approval/advise`、`RequestContext`、内核可执行动作闭集——这些全部留给 T7b。

## 问题

ME-3c 交付了回调通道本身（`agent24-os-proto::rpc`），但故意什么方法都不挂：生产环境唯一装配点 `agent24d/src/domain.rs:1207-1216` 写死 `Arc::new(|_| Methods::none())`；`rpc.rs` 模块文档原话「the first handler (ME-3d/3e) is where it gets its call site」。T7a 要把这条线从「永远空」变成「按模块清单声明的能力，真的授予、真的能用、未授予的调用得到明确的 `forbidden`」，并交付第一个真实方法：模块通过回调把一条事件转发到内核 WS 事件流，只能以自己的名义（不能冒充别的模块）。

## 现状（均已读源码验证，含行号；上一版文档在这几处的转述被 Codex 挑出过错误，这一版已订正）

- **`Offer`**：`agent24-os-proto/src/initialize.rs:100-119`，`pub struct Offer { pub provides: Vec<String> }`，`Offer::none()` 是空 `provides`。它是 `initialize` 握手结果里内核声明「我这次连接愿意应答哪些方法族前缀」的那一半（`Expectation.offer` → `InitializeResult.offer`，`initialize.rs:208-254`）。**生产环境唯一非测试调用点**在 `supervisor.rs:851`，硬编码 `Offer::none()`；测试代码里还有五处 `Offer::none()` 调用表达式（`initialize.rs` 三处、`endpoint.rs` 一处、`rpc.rs` 一处，Codex 第 2 轮核实的准确计数）。**没有任何生产代码构造过非空 `Offer`**。
- **`MountReport.granted`**：`agent24d/src/domain.rs`，`granted: Vec<String>` 字段。进程内路径真的算：`Grants::granting(manifest.kernel_capabilities(), KERNEL_GRANTS)`（`domain.rs:1002`，`KERNEL_GRANTS = &[Capability::Events, Capability::Memory]`，`domain.rs:70`），再构造出 `granted_names` 放进 `Mounted` 分支的 `MountReport`（`domain.rs:1074-1082`）。进程外路径（`mount_package`，`domain.rs:1107`）从不调用 `kernel_capabilities()`，报告构造闭包直接写死 `granted: Vec::new()`（`domain.rs:1121-1131`）。
- **清单模型是共享的，只是没被消费**：`RawManifest.kernel_capabilities: Vec<String>`（`agent24-domain/src/lib.rs:369`）解析成 `DomainOsManifest.kernel_capabilities: Vec<Capability>`（`lib.rs:414`，accessor `lib.rs:817-819`），校验走 `Capability::parse`（`lib.rs:198-206`，未知能力名报 `UnknownCapability`）。这条解析路径（`lib.rs:700-785` 一带）**进程内、进程外共用同一个 `DomainOsManifest::from_yaml`**，`impl_kind`/`spawn` 交叉校验就在同一函数里按 `InProcessCrate`/`OutOfProcessProvider` 分支（`lib.rs:753-769`）。也就是说：**进程外模块的清单今天已经可以写 `kernel_capabilities: [events]`，解析和校验都不会报错**——只是 `mount_package` 从来没读过这个字段。**上一版文档说“进程外模块要新增一个对等的清单字段”是错的**：字段已经在，缺的只是消费它的代码。
- **进程内 `Capability::Events` 授予后长什么样，是唯一现成先例**：`KernelCtx::events(&self) -> Option<&EventSink>`（`lib.rs:1027-1032`，doc「`None` when `Capability::Events` was not granted」）。授予判定：`Grants::granting`/`Grants::has`（`lib.rs:886-899`）。句柄构造：`let sink = granted.has(Capability::Events).then(|| EventSink::new(manifest, Arc::new(HubBroadcast(events.clone())) as Arc<dyn EventBroadcast>));`（`domain.rs:1046-1051`），只在被授予时才 `Some`。具体 `KernelCtx` 实现 `MemoryCtx`（`agent24d/src/os_memory.rs:703-711`）把这个 `Option<EventSink>` 原样存着、原样返回——**没有授予就没有句柄，不是「有句柄但内部拒绝」**。**上一版文档写 `KernelCtx::emit` 是错的**，仓库里不存在这个方法名。
- **`EventSink::emit` 的校验规则，T7a 要原样复用，不重新发明**：`lib.rs:986-1009`。`kind` 长度上限 96 字节（`MAX_KIND_BYTES`，`lib.rs:914`，超限 `lib.rs:991-996`）；点分小写语法由 `valid_kind`（`lib.rs:929-942`）手写扫描校验（不是正则），要求 ≥2 个非空 `[a-z0-9_-]` 段；`payload` 的类型是 `serde_json::Map<String, Value>`（不是 `Value`），从签名上就排除了非对象 payload（`lib.rs:989`，doc `lib.rs:978-981` 说明这是刻意的，不让非对象被静默转成 `{}`）。
- **`X-A24-*` 的请求侧/响应侧过滤是两个不同函数**：`strips_from_request`（`proxy.rs:161-199`）过滤客户端→模块方向；`strips_from_response`（`proxy.rs:202-232`）过滤模块→客户端方向（额外剥 `set-cookie`/`www-authenticate` 等，不剥 `X-Forwarded-*`）。两者都走 `sanitize()`（`proxy.rs:278-289`）这同一个通用实现，只是 `strip` 判定函数不同。**上一版文档只提了 `strips_from_request` 就下结论「响应也不会带出 `X-A24-*`」，源码引用不准确**——结论仍然对，但这一版把两个函数都写清楚。
- **回调的“连接身份”确实来自连接本身，不是自报**：回调通道（`rpc::serve`）绑定在某个 `Arc<Generation>` 上；`Generation::admit_callback(request_id: Option<&str>)`（`drain.rs:363-375`）按状态判定：`Starting` 拒、`Revoked` 拒、**`Running` 无条件 `Ok(())`（不查 `in_flight`，这一条已经用源码逐字核对过）**、`Draining` 时必须带一个仍在 `in_flight` 里的 `request_id` 才放行（没带 id 直接拒）。这个状态机是 ME-3b-5 已经交付并测试过的，T7a 直接复用，不新增字段、不新增校验路径。
- **`Decision` 类型**：定义在 `agent24-protocol/src/types.rs:368-376`；`agent24-policy` 只是 `use agent24_protocol::{..., Decision, ...}` 导入使用（`agent24-policy/src/lib.rs:23-25`），不在自己的 crate 里定义。T7a 不涉及 `Decision`（审批相关，留给 T7b），这里记录只是为了不在 T7b 里重犯上一版「写错类型归属 crate」的错误。

## 设计

### 1. 进程外授予：一次性在 mount 时算出，握手和方法注册共用同一份

新增常量（`agent24d/src/domain.rs`，与现有 `KERNEL_GRANTS` 相邻）：

```rust
/// 进程外模块目前只可能拿到的能力集合。比 `KERNEL_GRANTS`（进程内）窄——
/// `Memory` 的进程外形态（经代理访问 M-D store）不在 T7a 范围内，未来需要
/// 时单独评估，不要把进程内那份常量原样搬过来当作“反正超集无害”。
const KERNEL_OOP_GRANTS: &[Capability] = &[Capability::Events];
```

**`granted` 只在成功挂载（`Mounted`）时才计算和携带，`Refused`/`Degraded` 分支保持 `Vec::new()` 不变**（Codex 第 2 轮 High 2：`domain.rs:1121-1131` 那个 `report` 闭包同时服务 `Refused`——身份不符——和 `Degraded`——host/目录失败——两条路径；`MountReport.granted` 的既有契约是「没有活模块就没有能力」，这两条路径下模块进程根本没起来，granted 必须是空，不能因为「顺手把闭包默认值改了」而破例）。具体做法：不改 `report` 闭包本身的默认值，只在 `Mounted` 分支构造 `MountReport` 时另行传入算好的 `granted_names`：

```rust
// 仍然只在挂载真正成功、拿到 name/manifest 之后计算：
let granted = Grants::granting(manifest.kernel_capabilities(), KERNEL_OOP_GRANTS);
let granted_names = /* 与进程内 domain.rs:1074-1082 同款的 Capability -> &str 映射 */;
// 只有 Mounted 分支的 MountReport 用 granted_names；report(Refused, ..)/report(Degraded, ..)
// 两处调用点不变，闭包默认的 granted: Vec::new() 原样保留。
```

`granted`（`Grants`）连同模块名 `name`、一个可用于事件转发的广播句柄（`Arc<dyn EventBroadcast>`，与进程内共用同一个 `HubBroadcast(events.clone())` 构造方式——`mount_package` 目前的签名里没有 `events` 这个参数，需要跟进程内路径一样从 `mount_all` 往下传一层，`domain.rs:920` 一带那次调用点要一并改）一起，在挂载成功后**构造一次**：

```rust
let event_sink = granted.has(Capability::Events)
    .then(|| Arc::new(EventSink::new(&manifest, broadcast.clone())));
```

`EventSink::new` 接受 `&DomainOsManifest`（决定事件里 `module` 字段的来源——**同一个信任来源，进程内进程外一致**，不是回调 params 自报的任何值），构造一次后可以跨这个模块的多次重启复用（能力授予是清单层面的决定，跟进程重启无关）。

### 2. `Offer` 从「永远空」变成「按 `granted` 算出来」

`supervise()`（`agent24-os-proto::supervisor`）的签名加一个 `offer: Offer` 参数（普通值，不是闭包——`Offer` 不依赖某一次具体的 `Generation`，只依赖 mount 时算好的 `granted`，每次重启用同一份即可，不需要像 `MethodsFor` 那样按生成号动态构造）。`mount_package` 调用 `supervise(...)` 时传入：

```rust
let offer = if granted.has(Capability::Events) {
    Offer { provides: vec!["_a24/events/".to_owned()] }
} else {
    Offer::none()
};
```

`supervisor.rs:851` 的握手代码从硬编码 `Offer::none()` 改为使用传入的 `offer`。

**方法族前缀是 `"_a24/events/"`，不是精确方法名，这一点这一版不再留给实现阶段猜**（Codex 第 2 轮 Medium 2，已读 `initialize.rs:115-119` 的 `provides()` 实现核实）：

```rust
fn provides(&self, method: &str) -> bool {
    self.provides.iter().any(|p| method.starts_with(p.as_str()))
}
```

这是**无边界的原始 `starts_with`**，不是按 `/` 分隔的路径段前缀。所以 `Offer { provides: vec!["_a24/events/".to_owned()] }` 会把 `"_a24/events/emit"` 判定为已提供，但**也会**把一个假设未来存在的 `"_a24/events/emitter"` 误判为已提供——这不是安全问题（`Methods::get` 是精确字符串匹配，`Offer` 只是握手阶段给 SDK 的「值不值得调用」提示，不参与真正的授权判定），但要写清楚不能填精确方法名 `"_a24/events/emit"`（那样反而会在未来加同族其它方法时需要跟着改 `Offer`，违背「前缀」这个设计初衷），也不能假装这个前缀不会有相邻误判——判据里加一条覆盖这个已知的、无害的不精确性。

**`Offer` 的既有契约要跟着这一轮的用法一起明确改写，不能两边各说各话**（Codex 第 3 轮 Medium 1）：`initialize.rs:96` 现有 doc comment 把 `provides` 定义成「实际注册了 handler 的方法族，不存在『支持但被禁用』这种中间态」。但 §3 的设计是「`_a24/events/emit` 对所有进程外连接都注册 handler，未授权时 handler 内部返回 `forbidden`」——如果 `Offer` 继续按老定义（有没有 handler）来算，它就应该对所有连接都声明 `provides`，而不是只对被授予的连接声明；round 2 的三方一致性判据（第 10 条）要求的又是「`Offer` 只对被授予的连接声明」。**这一版明确把 `Offer` 的契约改写为「这条连接被授权、且调用大概率不会拿到 `forbidden` 的方法族」**（不再是「有没有 handler」），因为：
- 目前生产环境唯一处非空 `Offer` 构造就是 T7a 本身要新增的这一处，`Offer::none()` 之外没有任何生产代码依赖旧定义，改写没有兼容性代价（现状一节核实过的另外五处 `Offer::none()` 调用都在测试代码里，同样是空 `Offer`，不依赖任何非空语义）；
- 对调用方（SDK/模块作者）而言，「握手时就知道值不值得调用」远比「握手时知道内核代码里是否碰巧注册了这个符号」有用，后者是内核的实现细节，不该被协议暴露。

实现时需要同步改的不止 `initialize.rs:96` 一处（Codex 第 4 轮 Low 1 补全的清单）——以下几处现有 doc comment 在 T7a 落地后都会变成过时说明，需要一并订正或删除，纳入实现的验收范围：`initialize.rs:23`（「`Offer` 为空，取决于有没有 handler」）、`initialize.rs:106`（`Offer::none()` 的阶段性说明）、`initialize.rs:217`（「直到 ME-3d 都是空」）、`rpc.rs:18`（「生产环境没有业务方法」）、`rpc.rs:59`（「尚无方法接受数组参数」——这一条尤其要改，因为 T7a 之后事件方法的 `payload` 就是任意 JSON，含数组，且第 5 节的通用预算正是为了兑现这条注释本身预告的前置条件）。全部改成跟这份设计文档一致的措辞：方法始终注册，`Offer` 反映授权与否，`dispatch()` 层已经有通用体积预算。

### 3. `MethodsFor` 闭包：方法总是注册，能力门控在 handler 内部做，不在「注册与否」上做

**`_a24/events/emit` 对每个进程外模块的连接都注册，不按 `granted` 决定挂不挂**（Codex 第 2 轮 Medium 1：`rpc.rs:18-24` 的既有文档原话是「a handler exists but the caller lacks the grant → forbidden」，这是仓库自己在 ME-3c 就写下的、留给 ME-3d/3e 兑现的契约；SPEC 也要求「不支持」（方法不存在）和「未授权」（方法存在但这次调用没有权限）是两种不同的、可区分的失败。上一版「未授权时方法不存在，返回 `-32601`」的设计混淆了这两者，且如果 T7b 的 gate/advise 走 forbidden、events 却走 method-not-found，会给同一份协议留下两套不一致的规则）：

```rust
let methods_for: MethodsFor = {
    let name = name.clone();
    let granted = granted.clone(); // Codex 第 3 轮 Low 1：Grants 只有 Clone，没有 Copy，
                                    // 闭包会被多次调用（每次重启一代调一次），不能整体移动
    let event_sink = event_sink.clone(); // Option<Arc<EventSink>>：未授予时是 None
    Arc::new(move |generation: &Arc<Generation>| {
        // Codex 第 3 轮 Medium 2：限流桶必须在这里、每次闭包被调用时新建，
        // 不能在闭包外面建一次再 clone 共享——否则一个模块重启后的新一代会
        // 继承旧一代已经用光的配额，或者反过来内部状态被多代共享导致互相干扰。
        // 一次重启 == 一次新的 Generation == 一个全新的桶。
        let limiter = Arc::new(RateLimiter::new(EVENTS_RATE_CAPACITY, EVENTS_RATE_REFILL_PER_SEC));
        Methods::none().with(
            "_a24/events/emit",
            Arc::new(EventsEmitHandler {
                generation: generation.clone(), // Codex 第 2 轮 High 1：状态检查要用的就是这个，不能丢
                name: name.clone(),
                granted: granted.clone(),
                sink: event_sink.clone(),
                limiter,
            }),
        )
    })
};
```

`EventsEmitHandler` 拿到调用时先查 `self.granted.has(Capability::Events)`；没有 → `forbidden`（见第 6 节的错误契约），**不建任何审计/事件记录**。`Offer`（第 2 节）和 `MountReport.granted`（第 1 节）都已经只在真正授予时才分别声明/上报——三者按同一个 `granted` 计算出来，天然一致，判据 10 会直接断言这个一致性，不靠约定俗成。

### 4. `_a24/events/emit` 方法契约

Params（严格类型，未知字段整体拒绝，不是「忽略未知字段」——Codex 第 2 轮 Medium 3：serde 默认行为是忽略未知字段，必须显式加 `deny_unknown_fields`，否则「未知字段拒绝」这句话不成立）：

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsEmitParams {
    kind: String,
    payload: Map<String, Value>,
    request_id: Option<String>,
}
```

没有 `module` 字段——落地事件的 `module` 只能来自闭包捕获的 `name`，协议上就不给模块一个可以自报模块名的地方，从根源上排除「伪造模块名」这个输入面，而不是「接受了但检查完丢弃」。

处理（**顺序本身是这一版的修复点**——Codex 第 3 轮 Medium 3：限流不能排在生命周期准入之前，否则一个 `Draining` 状态下缺 `request_id`/带假 `request_id` 的非法调用会先把配额花掉，挤占同一模块里合法、仍 `in_flight` 的事件；正确顺序是「谁有资格调用」先于「谁被准入这次调用」先于「这次调用要不要计费」）：

1. 能力检查：`self.granted.has(Capability::Events)`，没有 → `forbidden`（终止，不进入后续步骤，不计入限流配额）。
2. `self.generation.admit_callback(params.request_id.as_deref())`——直接复用现状里描述的既有状态机：`Running` 时不论有没有 `request_id` 都放行（背景事件，不绑定任何具体代理请求，这是刻意保留的能力，不是遗漏——进程内 `EventSink::emit` 本来就没有「必须绑定一次调用」这个限制，T7a 不应该给进程外加一个进程内没有的额外限制）；`Draining` 时必须带一个仍然 `in_flight` 的 `request_id` 才放行（正在退出，只完成已经在做的事，不接受新的背景事件）。失败 → 返回该状态对应的拒绝（复用 `CallbackRefused` 已有的变体，映射见第 6 节，不新增错误类型的语义，只新增它们到 wire 的映射），**不计入限流配额**（一次被生命周期拒绝的调用不该消耗这个模块的配额，否则一个 draining 期间被拒绝的调用还能顺带做到「消耗配额从而影响后续合法调用」这种旁路效果）。
3. 速率预算检查（见第 5 节）：只对通过了前两步的调用扣费；超限 → `rate_limited`，不进入后续步骤。（体积/节点预算检查发生得比这更早——见第 5 节，它是 `dispatch` 层面对所有帧的通用检查，在方法分派、也就是在这个 handler 被调用之前就已经做完了，这里不重复做。）
4. **events 专属的 16 KiB 序列化字节上限检查（见第 5 节）**，在调用 `sink.emit` 之前做：`serde_json::to_vec(&params.payload).len()` 超过 16 KiB → `payload_too_large`，直接返回，**不调用 `sink.emit`**。这一步必须排在 `emit` 之前，不能等 `emit` 自身校验通过之后再查——`EventSink::emit` 是校验和广播合一的一次调用，校验通过就已经把事件推进了共享的 `EventsHub`，事后再查字节数已经来不及，起不到界定环形缓冲区常驻内存的作用。
5. 通过后调用 `sink.emit(&params.kind, params.payload)`——`EventSink::emit` 自己的校验（长度、语法、对象类型）原样生效，失败时把 `EventSink::emit` 自己的错误转成 `-32602 invalid params`（这是校验失败，不是权限失败）。成功后事件已经通过 `EventSink` 内部逻辑构造 `EventBody::Module(ModuleEventPayload{module: <manifest 名字>, kind, payload})` 并推给 `EventsHub`——这一步完全复用现有实现，T7a 不新写任何事件构造/广播代码。

**这个顺序和现有 RPC 框架已经定死的顺序是两层不同的事，别混着看**：`check_params`（`rpc.rs` 的 `Handler::check_params`，纯 schema 校验，`deny_unknown_fields` 那一层）永远先于 `handler.call()`（上面 1-5 步都在 `call()` 内部）运行，这是 `dispatch()` 自己的既有顺序（`rpc.rs:474-491`，先 `methods.get(method)` 找到 handler，再 `check_params`，最后才返回 `Dispatch::Call` 交给 `call()`），不是本设计新定的规则，也不是本设计能够改的。所以「未声明能力 + 畸形 params（比如带了未知字段）」永远先得到 `-32602`，因为 `call()`（能力检查在里面）根本没有机会被调用——这条优先级由既有框架保证，T7a 只是如实记录，不需要也不应该在 `call()` 里重新判断「万一 params 也有问题该怎么办」。

**已知、接受的一处窄竞态，如实写清楚，不假装堵上**（Codex 第 2 轮 Medium 6）：第 2 步的 `admit_callback` 是一次性快照检查，检查完锁就释放；如果检查通过后、`sink.emit()` 真正广播前，这一代恰好被 `revoke()`，事件仍然会被广播出去。这跟 `Generation::revoke()` 自己的文档语义是一致的——「让 in-flight 的请求跑完最多 `grace` 那么久」，撤销从来不是瞬时打断，本来就允许「信号发出的一瞬间还有工作在收尾」。事件是只读的、面向 WS 客户端的旁路信息，不驱动内核执行任何副作用，这条窄竞态的后果最多是「模块被要求停止之后，还多广播了一条它自己不知道自己快要被杀的事件」，可接受。**T7b 不能照抄这条**：gate 批准后是内核真的去执行一个动作，如果同样在「检查完锁就放」和「真正执行」之间开一个窗口，后果是「已经撤销的一代还能让内核代它做事」，这在 T7b 里必须是硬性禁止的，需要把状态检查和执行提交做成一次不可分割的操作（T7b 设计文档会专门处理，这里只是先把两者的差异写清楚，避免读者把 T7a 这条可接受的竞态错误地类推到 T7b）。

### 5. 解析开销与广播风暴：T7a 是第一个真正生效的业务方法，必须先兑现 `rpc.rs` 自己留的前置条件

`rpc.rs:48-59` 的既有文档原话：单帧上限 1 MiB，但解析后的 `Value` 可能膨胀到超过 15 MiB（`{"a":[0,0,…]}` 那种情况），64 路并发就逼近 1 GiB；**「在第一个方法接受数组类参数之前」必须按字节或按单次调用上限收紧**（FU-53 遗留）。T7a 正是那个第一个方法，不能绕开这条前置条件。

**上一版「解析中途提前中止」这个提法架构上不成立，这一版订正（Codex 第 3 轮 High 1，已读 `rpc.rs:371-497` 的 `dispatch()` 实现核实）**：`dispatch()` 对每一帧都先整体 `serde_json::from_slice::<Value>`（`rpc.rs:374`），这是一次通用解析，不区分方法，也不知道即将调用哪个 handler；`params` 再被整体 `clone()` 一次（`rpc.rs:447`）；`methods.get(method)` 找到 handler 之后才轮到 `Handler::check_params`（`rpc.rs:485-491`），它的签名是 `Result<(), String>`，失败被 `dispatch()` 固定映射成 `-32602`（`rpc.rs:489`），改不出一个独立的 `payload_too_large`。也就是说：**等轮到 `EventsEmitParams` 反序列化的时候，膨胀已经发生过一次、`clone()` 又原样复制了一次**，在这一层做「节点数限制的 `Deserializer`」根本拦不住任何东西。

**这一版把预算检查搬到正确的位置——`dispatch()` 自己，对所有方法通用，而不是某个方法的 `Handler::check_params` 里，并给出精确的插入点和计数定义（Codex 第 4 轮 Medium 1/Medium 2）**：

- **插入点**：不是「`rpc.rs:374` 解析完整帧后立刻」（那太早，早于 envelope 的一堆更便宜的合法性检查，会让预算检查抢在 `-32600`/重复 id 这些本该更先返回的错误前面）。正确位置是 `rpc.rs:445-449` 那个 `match obj.get("params")` 分支里、**`Some(p @ Value::Object(_)) => p.clone()` 这一行的 `.clone()` 之前**：此时 envelope 结构、重复 key、`id`（`MAX_ID_BYTES`/重复 in-flight 检查）、`jsonrpc` 版本、`method` 字段都已经验过（`rpc.rs:378-444`，这些检查都不涉及对 `params` 子树的遍历，开销和预算无关，让它们照旧先跑），而且 `params` 非对象的情况（`Some(_) => invalid_params`）已经在同一个 `match` 里被处理掉，不会被预算检查误判。在这一点上对 `obj.get("params")` **返回的引用**（不是 clone 出来的副本）做一次遍历，超限就直接返回，`.clone()`、方法查找、`check_params`、`handler.call()` 全部不再发生。
  - **超限时的 wire 行为，精确定义**（Codex 第 4 轮 Medium 1 的四个问题，答案由这个插入点本身决定，不是另外新定的规则）：此时 `id: Option<String>`（`rpc.rs:407-416`）已经提取好——**是请求**（`id` 是 `Some`）→ 回显这个已经验证过的合法 id，返回 `payload_too_large`（跟其它任何在这个位置之后失败的错误一样，走既有的 `fail()` 路径）；**是通知**（`id` 是 `None`）→ `Dispatch::Ignore`，不回任何响应（跟通知的其它失败一致，比如未知方法的通知也是 `Ignore`，不特殊）；**`params` 本身不是对象**（数组/标量）→ 走既有的 `-32602 invalid params`，预算检查根本不触及（这个分支在预算检查之前已经返回）；**envelope 里有重复顶层 `id`**（`rpc.rs:417-424` 的 in-flight 去重）→ 那一步比预算检查更早，天然优先。
- **计数定义，精确到可以直接实现和直接互相对拍测试结果（Codex 第 4 轮 Medium 2）**：对一个 `serde_json::Value` 递归定义 `(nodes, max_depth, string_bytes)`：
  - `Null`/`Bool`/`Number`：`nodes += 1`，不贡献 `string_bytes`。
  - `String(s)`：`nodes += 1`，`string_bytes += s.len()`（UTF-8 字节数）。
  - `Array(items)`：容器本身 `nodes += 1`，再对每个元素递归，元素的 `depth` = 容器的 `depth + 1`。
  - `Object(entries)`：容器本身 `nodes += 1`；对每个 `(key, value)`，**`key` 的字节数计入 `string_bytes`**（Codex 第 4 轮 High 1 的核心订正——上一版只数了 `Value::String`，漏了 object key，一个几百 KB 长的 key、value 是 `null` 的 payload 能绕过整个预算），`value` 递归，`depth` 同样是容器的 `depth + 1`；key 本身不单独计入 `nodes`（key 是 entry 的一部分，不是独立节点，避免「一个 entry 到底算一个节点还是两个」的歧义）。
  - 根节点 `depth = 0`。示例：`{}` → `(1, 0, 0)`；`{"a":1}` → `(2, 1, 1)`；`[1,2,3]` → `(4, 1, 0)`；`{"a":{"b":"hello"}}` → `(3, 2, 7)`（`"a"` 1 字节 + `"b"` 1 字节 + `"hello"` 5 字节）。
  - **具体阈值，这一版给死数字**：总节点数上限 **5,000**；最大嵌套深度上限 **32**；字符串字节总和（含 object key）上限 **262,144（256 KiB）**。超限统一返回 `payload_too_large`（第 6 节）。这三个数字是本轮的默认值，不是协议常量；调用方需要更大的 payload 时作为独立的后续变更单独放宽。
  - **这不是「解析中途中止」**（那需要重写 `serde_json` 用的 `Deserializer`，本轮不做，如实承认这一点），**是「解析完之后、扩散（`clone()`、交给 handler）之前立刻拦下」**——拦住的是额外的 `clone()`、以及把这份数据一直带到某个 handler、在这个调用的整个生命周期里持续占着内存（尤其是跟 64 路并发上限叠加时的持续占用）；单次解析那一刻的瞬时峰值内存本轮确实拦不住，这是本轮承认但不解决的残余成本，比「完全不拦」小得多。
  - 这个检查在 `dispatch()` 里对**所有**方法通用（不只是 `_a24/events/emit`），直接兑现 `rpc.rs:48-59` 那条「在第一个方法接受数组类参数之前」的前置条件，而不是把它变成 events 方法自己的特例——未来任何新方法都自动受益，不需要每个方法各自实现一遍。
- **256 KiB 通用预算不够界定共享事件总线，events 专属再加一道更紧的字节上限（Codex 第 4 轮 High 1 的第二部分订正）**：`dispatch()` 的通用预算是为了挡住「解析放大」这种一般性资源风险，阈值定得比较宽松（对任何方法都够用）。但 `_a24/events/emit` 特殊在它的 `payload` 最终会原样进入一个所有模块、所有 WS 订阅者共享的环形缓冲区（`EventsHub`，容量 4096 槽，`agent24d/src/events.rs:22`，慢订阅者只有 `Lagged` 或单次发送超过 10 秒才会被断开，`events.rs:75`）——如果每个事件都能顶着 256 KiB 通过，满环理论上限是 `4096 × 256 KiB ≈ 1 GiB`，这跟「一条被转发的事件」这个功能的实际体量完全不成比例。所以在 `EventsEmitHandler::call()` 里（不是通用 `dispatch()` 层），对已经通过 `dispatch()` 通用预算的 `payload`，在调用 `EventSink::emit` **之前**先检查一次**序列化后总字节数（`serde_json::to_vec(&payload).len()`）不超过 16 KiB**，超限返回 `payload_too_large`，`EventSink::emit` 根本不会被调用。**顺序是这一版实现阶段发现并订正的一处设计文档措辞错误**：早先的措辞写成「先过 `EventSink::emit` 自身校验、再检查 16 KiB」——但 `EventSink::emit` 是校验和广播合一的单一调用（校验通过就已经把事件推进了共享的 `EventsHub`），如果真按那个顺序做，一个超过 16 KiB 但仍在 256 KiB 通用预算内的 payload 会先被广播进环形缓冲区，然后才被这道检查拒绝——这道检查存在的目的（界定环形缓冲区的常驻内存）会被自己的顺序破坏掉。改成「先查字节数、超限直接拒绝、不调用 `emit`」才是这道检查唯一自洽的位置。`4096 × 16 KiB = 64 MiB`，是一个即使所有槽都塞满同一个模块能构造出的最大合法事件，也可以接受的常驻上限，不需要再引入一个全局聚合计数器。这个 16 KiB 检查只用于 events，不影响 `dispatch()` 通用预算对其它方法的适用性。
- **速率限制**：即使每次调用都在体积预算内，已授权模块仍然可以无限连续调用，把总线填满、把慢客户端挤下线。加一个每代（per-`Generation`）的令牌桶速率限制（`EventsEmitHandler` 里的 `limiter` 字段，第 3 节，每次生成新一代时新建，不跨重启共享）：**容量 20、每秒补充 5 个**——支持真实场景下的突发（一次性上报一小批事件），限制持续高频轰炸的稳态速率。超限时返回 `rate_limited`（第 6 节），不静默丢弃也不排队等待（排队本身就是要被界定的内存）。
  - **时钟与并发语义，精确定义（Codex 第 4 轮 Medium 3）**：令牌桶用单调时钟（`std::time::Instant`），每次调用时惰性结算——`refill = min(capacity, tokens + elapsed.as_secs_f64() * refill_per_sec)`，不是离散的「每 200ms 加一个」；检查剩余量是否 ≥1 和扣减这一个token，是**同一把锁/同一个原子操作内完成**的单一临界区，不是先查后减的两步（否则并发场景下会出现超发）。生产实现之外，测试需要一个可注入的时钟（不能靠真实 `sleep`，会 flaky）：冻结时钟后并发发起 64 次调用，断言严格得到 20 次成功、44 次 `rate_limited`（不多不少，验证原子性）；推进时钟 200ms（=1 个 token 的补充周期）后断言严格恢复 1 个可用配额。
  - **一次因为 `EventSink::emit` 校验失败（比如 `kind` 语法不对）而失败的调用，仍然消耗了限流配额**（Codex 第 4 轮 Medium 3 明确要求写清楚并测试）——处理顺序（第 4 节）里限流扣费排在 `emit` 校验之前，这是刻意的：如果校验失败不算数，一个调用方就能免费尝试大量不同的 `kind` 字符串组合而不受限流约束，这本身就是限流想防的一类行为。
- **明确不做、如实记录的限制**：这一版的速率限制和字节上限都是**按模块（per-`Generation`）**的，不是全局聚合的——今天生产 catalogue 只有一个进程外模块（sin90），多个模块各自独立限流已经能防住「一个模块单独作恶」，不存在「多个模块联合起来正好把总线打满」这个现实风险，所以本轮不做跨模块的全局预算聚合。**这是一个已知、当前可接受的缺口，不是遗漏**：一旦生产 catalogue 出现第二个互相不信任的进程外模块，需要在那之前补一个 `EventsHub`/daemon 级别的聚合预算，本文档不实现它，只记下这个前提条件。

**这两条不是「给事件加 approval_token」的理由**——第 7 节会解释，令牌防的是伪造/重放，防不住一个诚实获得授权的模块高频调用；这里要的是配额/预算机制，是完全不同的一类保护。

### 6. JSON-RPC 错误契约与成功结果的形状——这一版明确定义，不留给实现阶段猜

**新增错误 `kind`**（补进 `rpc.rs` 现有 `ErrorKind` 闭集，`rpc.rs:101-102` 一带；`Forbidden` **已经存在**于现有闭集里，是复用不是新增——Codex 第 3 轮 Low 2 订正了这一点，这一版跟着改口径：这一轮实际新增 5 个 kind（`not_ready`/`draining`/`revoked`/`rate_limited`/`payload_too_large`），复用 1 个既有的 `forbidden`）：

| 场景 | `kind` | JSON-RPC `code` | 在哪一层产生 |
|---|---|---|---|
| 能力未授予 | `forbidden`（复用既有变体） | 自定义 application error（非 `-32601`，方法确实存在） | `EventsEmitHandler::call()` 第 1 步 |
| `CallbackRefused::NotReady`（`Starting`） | `not_ready`（新增） | 同上 | `call()` 第 2 步 |
| `CallbackRefused::DrainingWithoutRequest` / `DrainingUnknownRequest` | `draining`（新增） | 同上 | `call()` 第 2 步 |
| `CallbackRefused::Revoked` | `revoked`（新增） | 同上 | `call()` 第 2 步 |
| 调用频率超出限流 | `rate_limited`（新增） | 同上 | `call()` 第 3 步 |
| 整帧解析后节点数/深度/字符串字节总和超限（第 5 节） | `payload_too_large`（新增） | 同上 | `dispatch()` 本身，**在方法查找、`check_params`、`call()` 之前**，对所有方法通用 |
| `EventSink::emit` 自身校验失败（kind 超长/语法不对） | 沿用现有 `-32602 invalid params` | `-32602` | `call()` 第 4 步 |
| params 带未知字段/缺字段/类型不对（含 `payload` 非对象——这类在反序列化阶段已被类型签名挡掉） | 沿用现有 `-32602 invalid params` | `-32602` | `dispatch()` 的 `check_params`，在 `call()` 之前 |

**错误优先级由既有 RPC 框架的调用顺序决定，不是本设计新定的**（Codex 第 3 轮 Medium 4）：`dispatch()` 的既有顺序是「整帧解析 → （本轮新增）体积/节点预算 → 找 handler（不存在→`-32601`）→ `check_params`（不合规→`-32602`）→ `call()`」（`rpc.rs:371-497`）。所以「能力未授予」永远不可能和「未知字段」同时出现——`call()` 里的能力检查根本没有机会跑到，畸形 params 已经先在 `check_params` 那一步被 `-32602` 拦下了。这条优先级只是如实记录既有框架行为，本设计不需要、也不应该在 `call()` 内部重新处理这种组合。

**成功结果**：固定返回 `{}`（空 JSON 对象），不返回事件的回显（模块自己知道自己发了什么，不需要内核回显；返回 `null` 也可以，选 `{}` 是为了跟未来其它方法的成功结果形状保持一致，方便调用方统一按对象解析）。判据里需要有一条**直接断言这个形状**，不能只测「调用成功了」（Codex 第 3 轮 Medium 4）。

### 7. 为什么不需要 `approval_token`

`_a24/events/emit` 不是在请求「许可去做某件事」，是在报告「一件已经发生的事」——它能造成的最坏后果是「一条被内核转发的、`module` 字段保真的事件」，而 `module` 保真已经由「协议里没有自报字段」这条设计保证了，不需要靠秘密令牌再加一层。`approval_token` 存在的理由（防止非法拿到令牌的一方冒充发起审批请求、防重放）在这里不适用：能调用这个方法的前提已经是「连上了这个模块自己的回调通道」，通道本身就是权限边界（同一 UID、模块专属 socket），而调用的后果只是「转发一条自己范围内的事件」，不涉及内核代为执行任何副作用。T7b 的 gate/advise 因为后果是「内核可能真的执行一个动作」或「用户可能真的做一个决定」，安全等级不同，才需要 `approval_token` 这层额外机制。

## 判据（带正对照）

1. 模块清单未声明 `events` 能力 → `_a24/events/emit`（方法本身存在，`Methods::get` 能找到 handler）调用得到 `forbidden`，**不是** `-32601`，不建任何事件。**正对照**：声明了的模块，同样的调用能成功并在 WS 上看到事件。
2. 未知字段：params 里出现协议未定义的字段（包括一个自称的 `module` 字段）→ 严格反序列化拒绝该调用（`-32602`），事件不落地。**正对照**：只带 `kind`/`payload`/可选 `request_id` 的合法 params 正常处理。（这一条只测协议层：带了不认识的字段就整个调用失败，不测「字段被忽略」。）
3. 归因来源测试（与判据 2 分开测，避免同一个输入既要求被拒绝又要求产生输出这种自相矛盾）：构造一次合法调用，其中 `payload`（业务数据，允许任意 key）恰好包含一个名为 `"module"` 的普通键，值是模块自己的业务数据 → 落地事件的**信封**（`ModuleEventPayload.module`）仍然等于闭包捕获的清单名字，`payload` 里那个同名 key 原样透传、不被内核读取或用来覆盖信封字段。**正对照**：`payload` 不含这个 key 时行为完全一致，证明信封字段的来源从始至终只有一个（闭包捕获），不依赖 `payload` 里有没有同名 key。
4. `kind` 超过 96 字节或不满足点分小写语法 → `EventSink::emit` 原有校验路径拒绝（`-32602`），事件不落地。**正对照**：合法 `kind`（如 `"task.transitioned"`）正常落地。
5. `payload` 不是 JSON 对象（数组/字符串/数字/`null`）→ 因为 params 类型本身就要求 `payload: Map<String, Value>`，这类输入在反序列化阶段就失败，属于判据 2 的一个特例，不需要独立的运行时检查代码，但需要一条测试覆盖它确实在反序列化阶段就被拒绝，而不是侥幸通过反序列化后在 `EventSink::emit` 里才被拒绝。
6. `Draining` 状态下、不带 `request_id`（或带一个已经不在 `in_flight` 里的 `request_id`）→ 拒绝，且错误 `kind` 精确等于 `draining`，事件不落地。**正对照**：`Draining` 状态下带一个仍然 `in_flight` 的 `request_id` → 正常落地，结果严格等于 `{}`。
7. `Running` 状态下不带 `request_id`（背景事件）→ 正常落地（这是刻意保留的行为，判据本身就是在确认「没有退化」，见设计第 4 节）。
8. `Starting` 状态 → 拒绝，`kind` 精确等于 `not_ready`；`Revoked` 状态 → 拒绝，`kind` 精确等于 `revoked`；两者都不论带不带 `request_id`。
9. 未被授予 `Events` 的模块清单里如果同时声明了一个不存在的能力名（比如打错字）→ `Capability::parse` 在清单解析阶段就报 `UnknownCapability`，模块整体挂载失败，不会走到「部分授予」这种中间态（这条不是新行为，是确认现有清单解析行为在这个新的消费点上没有被绕过）。
10. **一致性判据**：对同一个模块挂载结果，同时断言 `MountReport.granted`、真实 `initialize` 握手响应里的 `offer.provides("_a24/events/emit")`、以及能否成功调用该方法三者一致——都为「有」或都为「无」，不允许出现「方法能调用但 `granted` 是空」或者「`granted` 写着有但握手 `offer` 没声明」这类互相矛盾的组合。额外测：`Refused`/`Degraded` 结果的 `MountReport.granted` 必须是空（Codex 第 2 轮 High 2 直接要求的否定测试——防止「报告构造闭包的默认值被顺手改掉」这类回归）；模块重启（新的一代）后这三者依然一致（防止「只在第一次挂载时算对，重启后没跟着算」）。
11. `Offer.provides()` 的已知不精确性：`Offer{provides: vec!["_a24/events/"]}` 会把一个假设的相邻方法名（如 `"_a24/events/emitter"`）也判定为「已提供」，即使 `Methods` 里根本没有这个方法——这是文档记录过的、可接受的不精确（设计第 2 节），用一条测试固定这个已知行为，而不是让它成为未来被误当成 bug 修掉、或者被误当成安全边界的地方。
12. **`dispatch()` 层的体积/节点预算**（设计第 5 节，按 Codex 第 4 轮 High 1/Medium 1/Medium 2 订正后的精确定义）：
    - 用设计第 5 节给出的递归定义直接构造边界用例（不是猜一个「大概超限」的输入）：总节点数 5,000/5,001（`limit`/`limit+1`）、嵌套深度 32/33、字符串字节总和（**含 object key**）262,144/262,145 各测一次，超限的返回 `payload_too_large`，未超限的正常处理。
    - **object key 计入字节预算**（Codex 第 4 轮 High 1 的核心订正）：单独测一个「key 接近 256 KiB、value 是 `null`」的 payload——节点数和深度都很小，只有 key 字节能触发预算，必须同样被 `payload_too_large` 拦下，证明预算不是只数 `Value::String` 漏了 key。
    - **超限的 wire 行为按 id 分叉**（Codex 第 4 轮 Medium 1）：带合法 `id` 的请求超限 → 响应里回显同一个 `id`、`payload_too_large`；不带 `id` 的通知超限 → `Dispatch::Ignore`，完全没有响应产生。
    - 这个检查对**任意方法名**（包括一个压根不存在的方法名）都生效，用一条测试证明它不是 `_a24/events/emit` 专属的特例，且发生在方法查找/`check_params`之前（超限时不出现 `method_not_found`/`invalid_params`）。**正对照**：三项都在阈值以内的帧（哪怕 `kind`/`payload` 内容不合法）能正常走到 `check_params`/`call()`，在那里按各自的规则被处理，不会被预算检查误杀。
13. **events 专属的 16 KiB 序列化字节上限**（设计第 5 节）：构造一个通过了 `dispatch()` 通用预算、也通过了 `EventSink::emit` 自身校验，但 `serde_json::to_vec(&payload)` 超过 16 KiB 的合法调用 → `EventsEmitHandler` 返回 `payload_too_large`，事件不落地、不进入 `EventsHub`。**正对照**：16 KiB 以内的合法 payload 正常落地。
14. 单个 generation 短时间内高频调用 `_a24/events/emit`（容量 20、每秒补充 5 个，冻结的可注入时钟）→ 同一时刻并发发起 64 次调用，严格得到 20 次成功、44 次 `rate_limited`（验证扣减是原子的，不是「大概限流住了」）；推进时钟 200ms 后严格恢复 1 个可用配额。**正对照**：低于补充速率的持续调用不触发限流。额外测：一次因能力未授予（`forbidden`）或生命周期拒绝（`not_ready`/`draining`/`revoked`）而失败的调用，不消耗限流配额（设计第 4 节的顺序要求）；一次因 `EventSink::emit` 校验失败（比如非法 `kind`）而失败的调用，**仍然消耗了**限流配额（设计第 5 节，刻意如此，防止免费试探）；模块重启后新一代的配额从满额开始，不继承旧一代已经用掉的部分（Codex 第 3 轮 Medium 2）。
15. 成功结果的形状：任意一次成功的 `_a24/events/emit` 调用，JSON-RPC `result` 字段精确等于 `{}`，不多不少（Codex 第 3 轮 Medium 4，此前没有判据直接断言这一点）。
16. 组合失败的优先级：构造一个「能力未授予 **且** params 带未知字段」的调用 → 得到的是 `-32602`（`check_params` 先于 `call()`），不是 `forbidden`——这条判据的价值不是「测出一个新行为」，是把设计第 6 节记录的既有框架顺序用一个真实测试钉死，防止未来有人改动顺序时没注意到这里有个隐含依赖。

## 不改的东西

- `Generation`/`drain.rs` 的状态机、`admit_callback` 的判定逻辑——原样复用，不新增字段、不新增变体。
- `EventSink`/`EventBody::Module`/`ModuleEventPayload`——原样复用现有构造与校验，不重新实现。
- 进程内路径（`KERNEL_GRANTS`、`MemoryCtx`、`os_memory.rs`）——不改，`KERNEL_OOP_GRANTS` 是进程外专用的新常量，两者独立。
- `Capability` 枚举本身——T7a 不新增变体（`Capability::Approval` 留给 T7b 添加，那时才有消费它的代码）。
- `approval_token`、`ModuleApproval`、`_a24/approval/gate`、`_a24/approval/advise`、`RequestContext`——全部是 T7b 的范围，本文档不设计。
- `Capability::Memory` 的进程外形态——不在 `KERNEL_OOP_GRANTS` 里，未来若要开放，需要单独评估经代理访问 M-D store 的语义，不是本轮的隐含副产品。
- `report` 闭包（`domain.rs:1121-1131`）本身的默认值和 `Refused`/`Degraded` 两条调用路径——不改，只在 `Mounted` 分支额外传入算好的 `granted_names`（设计第 1 节，Codex 第 2 轮 High 2）。
- 「回调检查通过后、真正执行前这一代被撤销」这条窄竞态——T7a 明确接受（设计第 4 节），不引入新的原子性机制去堵它；T7b 不能援引这条先例，需要自己的原子提交设计。

---

## v1 → v2 改动（Codex 第 2 轮：0 Critical、4 High、6 Medium、2 Low，全部采纳）

- High 1：`MethodsFor` 闭包示例改为捕获真实 `generation: Arc<Generation>`（不再是丢弃的 `_generation`），`EventsEmitHandler` 携带 `generation` 字段，判据 3/6/7/8 才可能被实现验证。
- High 2：`MountReport.granted` 的计算收窄到只在 `Mounted` 分支生效，`report` 闭包默认值和 `Refused`/`Degraded` 调用点不变；新增判据 10 的否定测试。
- High 3：新增设计第 5 节，兑现 `rpc.rs:48-59` 自己留下的前置条件——解析阶段的体积/深度上限（`payload_too_large`）与按 generation 的调用速率限制（`rate_limited`），新增判据 12/13。
- High 4：判据新增第 10 条（Offer/Methods/MountReport.granted 三者一致性 + 重启后一致 + Refused/Degraded 恒空）。
- Medium 1：`_a24/events/emit` 改为对所有进程外模块无条件注册，能力门控在 handler 内部做（未授予 → `forbidden`），不再用「方法不存在」表达「未授权」；新增设计第 6 节定义完整的 JSON-RPC 错误契约（`forbidden`/`not_ready`/`draining`/`revoked`/`payload_too_large`/`rate_limited` 六个新 kind，成功结果固定 `{}`）。**〔这句「六个新 kind」的计数后来被订正——`forbidden` 其实是复用既有变体，实际新增 5 个；订正记录见下面「v2 → v3」Low 2〕**
- Medium 2：写清楚 `Offer.provides()` 是无边界 `starts_with`（已读 `initialize.rs:115-119` 核实），前缀必须是 `"_a24/events/"` 带尾斜杠而不是精确方法名，并把这个已知的相邻误判写成判据 11。
- Medium 3：`EventsEmitParams` 示例补上 `#[serde(deny_unknown_fields)]`。
- Medium 4：原判据 2/3（一个输入不能既要求被拒绝又要求产生输出）拆成判据 2（协议层未知字段拒绝）和判据 3（归因来源单元测试，用 `payload` 里一个普通同名 key，不是拒绝场景）。
- Medium 5：新增设计第 6 节的错误契约表和成功结果形状，不再留给实现阶段自己定。
- Medium 6：设计第 4 节新增一段，明确写清「检查通过后被撤销仍可能广播」是接受的窄竞态、为什么可以接受、以及为什么 T7b 不能照抄这个结论。
- Low 1：`Offer::none()` 出现次数的表述订正为「测试代码五处调用表达式（`initialize.rs` 三处、`endpoint.rs` 一处、`rpc.rs` 一处）」。
- Low 2：写清楚 `mount_package` 需要新增 `events`/广播句柄参数、从 `mount_all`（`domain.rs:920`）往下传一层。

---

## v2 → v3 改动（Codex 第 3 轮：0 Critical、2 High、4 Medium、2 Low，全部采纳）

- High 1：推翻「解析中途中止」的架构（已读 `rpc.rs:371-497` 的 `dispatch()` 核实，`Handler::check_params` 排在整帧解析和 `params.clone()` 之后，改不出独立错误 kind，也拦不住已经发生的膨胀），改为在 `dispatch()` 自己、整帧解析完成后立刻做一次通用遍历（节点数/深度/字符串字节总和），对所有方法生效，不是 events 专属；设计第 5 节给出具体数字（节点 5,000、深度 32、字符串字节 256 KiB），不再留白。
- High 2：速率限制给出具体数字（容量 20、每秒补充 5 个），限流桶改为每次 `MethodsFor` 闭包被调用时新建（对应 Medium 2），明确记录「本轮只按模块限流、不做跨模块全局聚合」是已知、当前可接受的缺口而非遗漏。
- Medium 1：改写 `Offer` 的既有契约（本文档内 + 实现时需要同步改 `initialize.rs:96` 的 doc comment）——从「有没有 handler」改成「这条连接是否被授权调用」，并订正问题陈述/背景两处仍写着「未授予的方法不存在」的旧措辞，使其与设计第 3 节一致。
- Medium 2：限流桶的构造挪到 `MethodsFor` 闭包**内部**（每次调用即每次生成新一代都新建一个），不再在闭包外构造后 `clone()`；判据 13 新增「重启后配额从满额开始，不继承旧一代」的测试。
- Medium 3：处理顺序改为「能力检查 → 生命周期准入 → 限流扣费 → emit」（原来是「能力 → 限流 → 生命周期」，会让生命周期本该拒绝的调用先消耗配额）；被能力/生命周期拒绝的调用不计入限流配额。
- Medium 4：判据 6/8 补上精确 `kind` 断言（`draining`/`not_ready`/`revoked`），新增判据 14（成功结果严格等于 `{}`）和判据 15（能力未授予 + 畸形 params 的组合优先级，由既有框架顺序决定，不是本设计新定的规则，见设计第 6 节新增的说明段）。
- Low 1：`MethodsFor` 示例里 `granted` 从直接移动改为 `granted.clone()`（`Grants` 只有 `Clone`，没有 `Copy`，闭包会被多次调用）。
- Low 2：changelog 措辞订正为「五个新增 kind（`not_ready`/`draining`/`revoked`/`payload_too_large`/`rate_limited`）、复用一个既有 `forbidden`」（上一版 changelog 错误地说成六个新增），并订正「判据 3/6/7/8」的引用为「判据 6/7/8」（判据 3 是归因测试，跟 generation 捕获无关）。

---

## v3 → v4 改动（Codex 第 4 轮：0 Critical、1 High、3 Medium、2 Low）

- High 1：256 KiB 字符串预算漏了 object key（一个几百 KB 长 key、value 是 `null` 的 payload 能绕过），设计第 5 节的计数定义订正为「字符串标量 + object key 的字节都计入」，并给出精确递归定义和示例；另外给 events 单独加一道 16 KiB 序列化字节上限（新增判据 13），把共享事件总线（4096 槽）的最坏常驻内存从「理论上到 GiB 级」压到 `4096 × 16 KiB ≈ 64 MiB`，不需要再引入一个全局聚合计数器。
- Medium 1：把预算检查的插入点从「解析完整帧后立刻」精确到「`rpc.rs:445-449` 的 `params` 类型判定之后、`.clone()` 之前」，并按已提取的 `id` 精确定义超限时的 wire 行为（请求回显 id、通知 `Ignore`、非 object params 和重复顶层 id 的优先级由更早的既有检查决定）；判据 12 补上「超限时不出现 method_not_found/invalid_params」和 id/notification 分叉的测试。
- Medium 2：给出体积/深度/字节计数的精确递归定义（含 3 个带期望值的例子），不再是一句「各算一个」的模糊描述；判据 12 按这个定义补齐 `limit-1/limit/limit+1` 边界测试。
- Medium 3：限流令牌桶钉死时钟语义（单调时钟、惰性结算、检查与扣减同一临界区）、要求测试用可注入时钟而不是真实 `sleep`、新增 64 路并发的原子性测试（判据 14），并明确写清「`EventSink::emit` 校验失败仍消耗配额」是刻意行为、需要被测到。
- Low 1：列全需要跟着改的 doc comment（不止 `initialize.rs:96`，还有 `initialize.rs:23/106/217`、`rpc.rs:18/59`），纳入实现范围。
- Low 2：订正「新增 4 个 kind」为「5 个」；`v1 → v2` 历史条目里「六个新 kind」的过时计数原样保留（历史记录不回改），加一句内联提示指向订正处；「另外五处非空构造」订正为「五处 `Offer::none()` 调用」（本身就是空构造，不是非空）；`第 111 行` 这类会漂移的行号自引用改成按描述指代，不用行号。

---

**本轮（v4）不再送 Codex 第 5 轮评审**：round 4 只剩 High 1 一条（已修）和三条 Medium/两条 Low（均已修），没有新的架构级不确定性——已经过了 4 轮，每轮都是净改进但代价已经足够高，接下来转入实现，让编译器和单元测试（尤其是判据 12/13/14 这几条本来就要求写成可执行测试）去验证这一版的细节是否真的自洽，而不是再开一轮纯文字评审。

---

**实现阶段发现并订正的一处 v4 文档错误（不是新的 Codex 评审轮次）**：第 4/5 节里「16 KiB 检查排在 `EventSink::emit` 自身校验之后」的措辞是错的——`emit` 校验通过即广播，事后检查已经堵不住共享 `EventsHub` 的内存风险；实现按「先查字节数、超限就不调用 `emit`」这个唯一自洽的顺序做的，文档已同步订正为一致的顺序。这是实现阶段自己发现的文档问题，不是外部评审发现的，记在这里避免以后对不上实现。

（v4（含上述订正），设计冻结，已进入实现；上一版 v1 的审批部分内容已拆分至 T7b，未来另开文档）
