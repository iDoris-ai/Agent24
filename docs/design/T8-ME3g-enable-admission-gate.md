# T8 / ME-3g —— 启用路径准入校验：`enable` 一个准入被拒的模块，不再假装成功

> 状态：**定稿 v7**（v1→v2 第 1 轮 0/1/5/2；v2→v3 第 2 轮 0/2/2/2；v3→v4 第 3 轮 0/4/5/2；v4→v5 第 4 轮 0/0/3/2；v5→v6 第 5 轮 0/1/1/2；v6→v7 第 6 轮 0/0/0/2，Critical/High/Medium/Low，每轮全部采纳，见文末）。第 6 轮只剩 2 条 Low（都是纯文档一致性/分支覆盖的收尾问题，不影响正确性），改完直接定稿，不再送第 7 轮——机制本身（目录匹配 provenance、名字+交付方式两条检查、恰好一条同名报告才触碰、根目录/包子目录两层 fail-closed）已经过 6 轮独立核实，无 Critical/High 遗留。累计 0 Critical / 8 High / 16 Medium / 12 Low，全部采纳。可以开始写代码。来源：`SPEC-ME3-OUT-OF-PROCESS.md` §7「F4e：`os enable` 会从『今天不可达』变成现实问题」+ §8 交付表 `ME-3g`；`SPEC-ME-FOLLOWUPS.md` F4e。`tasks.md`「四、主线到发布」`T8`，依赖 `ME-3a`（已交付，PR #156-161）。**v3 范围比 v1/v2 扩大**：第 2 轮 Codex 指出 v2 只挡「当前已经是 `Refused`」的模块，挡不住 F4e 最常见的触发路径——「当前 `Disabled`、清单其实早就对不上、但从没被检查过」——用户 2026-09-16 裁决把这个口子也关上。**v4 是这个扩大范围之后的第一次真正可实现的版本**：第 3 轮 Codex 指出 v3 的重扫机制本身有真实的语义错误（拿现成判定去比对一个会话过时的基准，且没法可靠区分「编译进内核」和「装过但现在找不到了」），本节把机制换了一种更简单、更站得住脚的做法，见「决策记录」和「设计」段。

## 问题

`MountOutcome`（`domain.rs:263-281`）有四种：`Mounted` / `Disabled` / `Degraded(why)` / `Refused(why)`。`Refused` 是「这次启动没有为它挂任何路由」的准入拒绝——今天已知的产生原因（`mount_all`/`mount_package`，`domain.rs:802-1235`）：目录/清单层面的（空 catalogue 版本、非法模块名、重名、占用内核保留路径段）、catalogue 记的版本跟已装清单自己声明的版本对不上、已装清单自称的交付方式（编译进内核 vs 进程外）跟它实际的安装形态矛盾（**v3 → v4（Codex 第 3 轮 Medium 5，措辞订正）**：这段开场话本身就是本设计一直在修正的那份不准确清单，跟下面「问题」段第二部分的分层表格核对过之后已经对齐——不是「缺 spawn 字段」，也不包括「缺依赖」，这两条从未产生过 `MountOutcome::Refused`，仍然是完全独立、这次关不上的口子，见「决策记录」）。这个判定**只在 daemon 启动时算一次**（`mount_all` 调用点 `server.rs:1205`），结果存进 `AppState.os_reports`，daemon 存活期间不重新算，也从不落盘——**重启是真的会重新算**：每次启动都重新跑 `with_discovered`/`discovery::scan` 建 catalogue、重新跑 `mount_all`，产生一份新的报告数组装进 state（`server.rs:1285`，核实过没有任何跨启动缓存）。

`agent24 os list`（`GET /api/v1/os` → `os_routes.rs` `view()` 第 146 行）已经如实把它渲染成 `state: "refused"`、`detail: Some(why)`。**但 `PATCH /api/v1/os/{name}`（`os enable`）今天完全不看这个判定**——`patch_os`（`os_routes.rs:577-686`）只检查名字是否已知（未知名字才 404），之后不管 `os_reports` 里这个名字的 `outcome` 是什么，一律往下走到 `apply()`（438-480）→ `OsConfig::set_enabled`（`os_config.rs:171-183`）把 `enabled: true` 原子写进 `os.json`，回 200。一个被准入拒绝的模块，`enable` 会成功、会落盘、`os list` 从此显示「已启用，但状态是 refused」——却**没有任何一条路径能让它变成活的**：不会重新准入，不会挂载，不会有路由。这正是 `SPEC-ME-FOLLOWUPS` F4e 记的账：当时（生产 catalogue 只有 sin90 且可准入）不可达，ME-3 引入进程外模块之后，「版本不符/清单非法」是会真实发生的拒绝原因，账到期了（**v4 → v5（Codex 第 4 轮 Low 1）**：SPEC 原文这句话把「缺依赖」也一起点了名，但下面的表格核实过——`requires_deps` 从来没被任何代码消费过，从来不是一个会真实发生的拒绝原因，这句话只抄 SPEC 原文列出的、今天确实存在的那两类，不照抄第三个还没兑现的）。

**v1 → v2（Codex 第 1 轮 High 1，范围核实）**：v1 核实过「SPEC 原文列的四类拒绝原因」现状，但把「清单非法」笼统地算成「今天已经在拒」，不够精确，且遗漏了一种更早的拒绝层。**v2 → v3（Codex 第 2 轮 High 2，进一步修正「版本不符」「清单非法」两格的具体机制）**：v2 的表格仍然把几种不同机制混在一起写——「版本不符」其实是三种彼此无关的检查（清单 schema/`min_daemon_protocol` 层面的版本、catalogue 与已装清单自身的版本、握手协商时的版本区间），「清单非法（自相矛盾）」也不是「缺 `spawn` 字段」（那种清单连解析都过不了，见下面第二行），而是「已装清单自己的 `impl_kind` 跟它被安装的方式（进程外 package vs 编译进内核）对不上」。逐项核实后，SPEC 点名的四类拒绝原因，实际分布在**三个不同的层**：

| SPEC 说法 | 实际发生的层 | 今天的行为 | 这次设计能不能碰到它 |
|---|---|---|---|
| **版本不符**（唯一算数的那种：catalogue 条目记的版本 vs 已安装清单自己声明的版本不一致） | `mount_package`（`domain.rs:1139-1148`，进程外）/ 编译进内核分支的等价检查（`domain.rs:955-967`）——**只在这个名字当前是「准入判定会真的跑到这一步」时才会被检查到** | 产生 `MountOutcome::Refused`，进 `os_reports` | ✅ 能，但只对**读现成判定**这一半（一个当前已经是 `Refused` 的名字）成立——**v3 → v4（Codex 第 3 轮 High 1）**：对进程外模块，`enable` 时**现场重扫**关不上这条：catalogue 身份就是从当时扫到的清单直接派生的，两者天然一致，这条检查对一个正常发现的包本来就不会触发，拿「上次启动记的旧版本」当基准比对现在的清单，比出来的是假阳性，不是真判定，见「决策记录」 |
| **版本不符**（清单 schema/`min_daemon_protocol` 层面，跟 catalogue 无关，纯粹是这份清单自己写没写对） | `DomainOsManifest::from_yaml`（`agent24-domain/src/lib.rs:606/632`），比清单解析更早的一步 | 解析失败的包根本进不了 catalogue，见下一行 | ⚪ 不需要碰——归入下一行 |
| **版本不符**（握手协商时双方版本区间取交集为空） | 运行期，`agent24-os-proto` 的握手逻辑，产生 `FailureKind::Refused`（`failure.rs:159`），**已经 `Mounted` 之后**才会发生 | 渲染成运行时的 degraded 状态，跟 `MountOutcome` 完全是两回事 | ⚪ 不相关——发生在挂载成功之后，不是准入 |
| **清单非法**（YAML/schema 解析失败） | `discovery::scan` 阶段（`DomainOsManifest::from_yaml`，`agent24-domain/src/lib.rs:741` 起的 `deny_unknown_fields`/`SpawnCommand::validate`）——解析失败的包**根本进不了 catalogue**，只是被 `with_discovered`（`server.rs:1520`）记进 `discovery::Scan.refused` 并打日志，从未变成一条 `MountReport`，`os_reports` 里连这个名字都没有 | `enable` 这个名字今天已经是「未知名字 → 404」（`os_routes.rs:585-592`），**已经符合 F4e「返回错误且不落盘」的要求** | ⚪ 不需要碰——已经是对的 |
| **清单非法**（已装清单**自己内部**自洽，但它的交付方式跟安装它的方式对不上——比如清单自称编译进内核，却是以进程外 package 的形式装在磁盘上） | `mount_package`（`domain.rs:1149-1160`），同一层；**这不是「缺 spawn 字段」**——一份能通过 schema 校验的进程外清单必然带 `spawn`，这里检查的是交付模式（`is_mountable_in_process()`）跟安装形态是否矛盾 | 产生 `MountOutcome::Refused` | ✅ 能——同上 |
| **spawn 目标不存在**（`spawn` 字段本身合法，但它指向的可执行文件在磁盘上不存在） | 从未在准入时检查——`launch::resolve()`（`launch.rs:156-186`）只在监督者真的要 spawn 那一刻才解析，解析失败是运行期的 `FailureKind::Setup`（FU-57），不产生 `MountOutcome::Refused` | 今天 `enable` 一个「装好了但 spawn 目标缺失」的模块会成功落盘——**这正是 F4e 描述的坏状态，本设计关不上这个口子** | ❌ 不能——需要在准入时新加一次路径解析，独立工作量，见下 |
| **缺依赖** | 从未实现——`RawManifest.requires_deps`（`agent24-domain/src/lib.rs:363`）解析进了清单结构，**零处消费** | 同上，`enable` 会成功落盘 | ❌ 不能——「依赖」指什么、去哪查都还没设计，独立工作量 |

## 决策记录

**v2 → v3（Codex 第 2 轮 High 1，用户 2026-09-16 裁决：扩大范围）**：v2 只读 `os_reports` 里现成的判定，遗漏了一个关键场景——`mount_all` 的准入判定是「先判身份（重名/保留名），再判用户开关，最后才判清单」（`domain.rs:878` 附近的 `!registry.is_ok_and(|c| c.is_enabled(&name))` 这一步在清单相关检查**之前**就 `continue` 了），这是刻意设计（`identity_admission_beats_disabled_but_manifest_admission_cannot` 测试的原话：「一个被禁用的模块故意从不被构造——这样一个坏构造器才不会拖垮 daemon」）——**代价是**：一个当前 `Disabled` 的模块，它的清单是否跟 catalogue 自洽，daemon 从来没检查过，`os_reports` 里它的 `outcome` 只会是 `Disabled`，不会是 `Refused`。这正是 F4e 最常见的触发方式：一个进程外模块本来就装着一份写错的清单，操作者从没打开过它（一直是默认关闭），某天 `enable`——v2 的门只看「有没有已经是 `Refused` 的报告」，这里没有，直接放行、成功落盘，直到下次重启才发现根本起不来。**只读现成判定关不上这个口子。**

裁决：**关上它，但只对进程外（`Package` 安装）模块关，编译进内核的模块明确关不上**——分界线是能不能在不构造/不 spawn 的前提下重新核对：

- **进程外模块能**：清单在磁盘上是一个独立文件，`discovery::scan`（`agent24-os-packages/src/discovery.rs:85`）本来就是「只读文件、解析 YAML」的纯函数，不碰进程、不碰监督器，重新跑一次不违反「disabled 不构造」这条安全线——因为它压根不构造任何东西。
- **编译进内核的模块不能**：它的「清单」来自调用 `build()` 这个闭包本身的副作用（`domain.rs:935` 附近），要重新核对就得真的调用它——那正是「disabled 不构造」这条安全线存在的原因（一个坏构造器可能挂起或崩溃 daemon）。检查不了就是检查不了，这是架构本身逼出来的限制，不是这次偷懒——继续放在「本设计关不上」那一类，跟 spawn 目标/缺依赖一样记成独立跟进项。

**v3 → v4（Codex 第 3 轮 High 1，重扫比对的基准错了）**：v3 拿现场重扫出来的清单去比对 `report.version`——这条 `Disabled` 报告在**上一次启动**时记下的 catalogue 版本。这个基准选错了：对进程外模块，catalogue 的身份信息（name/version）从来不是独立存在的东西，`with_discovered`（`server.rs:1528`）就是直接拿扫描出来的清单本身去建 catalogue 条目——**每次启动，catalogue 都会跟当时磁盘上的清单自动保持一致**，`mount_package` 那条「catalogue vs 清单」的版本检查，对一个正常发现的包来说几乎不可能触发（只有 catalogue 是手搭的——比如编译进内核的条目——才会跟清单独立开）。所以：如果操作者在模块被禁用期间，把磁盘上的包从 v1 升级到 v2，下一次重启会拿 v2 建一份全新的 catalogue 条目，v2 对 v2 自己，永远一致，根本不会被拒——但 v3 的重扫拿「上次启动记的 v1」去比对现在扫到的 v2，会**误判**成「版本不符」挡住一次完全正常的 enable。v3 文档里「下一次重启一定会是 `Refused`」这句断言不成立，判据 9 因此把一个假阳性写成了「应该发生的行为」。**改法：重扫只检查清单自己内部是否自洽（交付方式跟安装形态矛不矛盾），不再跟任何「记住的」catalogue 值比对版本或名字**——这也顺带解决了 v3 里「先按名字找清单、再检查名字是否相等」这种逻辑上不可能触发的死代码（`domain.rs` 意义上按名字过滤之后，「名字不等」这个分支必然为假）。

**v3 → v4（Codex 第 3 轮 High 2，判不出「编译进内核」还是「包不见了」）**：v3 说「扫描找不到清单 = 编译进内核，放行」——不成立，`MountReport` 没有记录这个名字当初是 `Build::Package` 还是 `Build::InProcess`，「扫描找不到」这五个字可能是：真的编译进内核；包在 daemon 启动之后被卸载/改名了；清单现在坏掉、解析失败，落进了 `scan.refused` 而不是 `scan.found`（`discovery.rs` 的 `Scan` 结构区分这两类）；`packages_root` 本身不可读。这几种情况混在一起当「放行」处理，会把「包明明装过、现在肯定起不来」也悄悄放过去——跟本设计要关的口子背道而驰。**改法：不去 `MountReport` 里加字段（那是一个被很多既有测试大量构造的结构，改它的形状代价不小），改成在 `AppState` 单独存一个更小的东西**——见下方「设计」段新增的字段（这一轮定的名字是 `package_names`，第 4 轮 Medium 1 之后改成了 `package_dirs`，见「设计」段的最终形状）。

**本设计因此关上 SPEC 四类原因里的两类半**：「版本不符（catalogue vs 已装清单）」「清单自相矛盾」两类——**前者只在这个名字当前判定已经是 `Refused` 时能查到**（`enable` 前读现成判定这一半）；`Disabled` 的进程外模块走的是「现场重扫」那一半，**不跟任何「记住的」catalogue 值比对版本**（理由见「设计」段的重扫小节——对一个正常发现的包，版本比对对着现在的清单几乎必然通过，拿旧记录当基准反而是假阳性），但**会**比对清单现在声明的名字跟请求的名字（`os.json` 的配置键本身，不是某个记住的旧值）是否一致，再检查交付方式自洽性——**v5 → v6（Codex 第 5 轮 High 1）**补回的那条名字检查，见「设计」段。「清单解析失败」这一类本来就对；「spawn 目标不存在」「缺依赖」两类，以及「编译进内核模块的 Disabled→enable」这个子情形，明确关不上，写进 `tasks.md`/`followups.md` 的独立跟进项（比如 `FU-6x`），不能让 `DONE` 掩盖它们仍然开着。FU-61 在同样的位置做过同样性质的收窄（「本轮只做清单摘要/目录核对，不做整包哈希」），这里照做。

## 设计

### 在哪加、加什么

`patch_os`（`os_routes.rs:577-686`）里，`update: DomainOsUpdate`（含 `enabled: bool`）解析出来之后（600 行之后）、在获取 `path`/拿 `os_control` 锁（607-622）之前，插入一道门。

**v1 → v2（Codex 第 1 轮 Medium 2，重名语义）**：v1 用 `.find(|r| r.name == name)` 取「第一条同名报告」，当成这个名字唯一的判定——不成立。核实 `mount_all` 的循环（`domain.rs:802-860`）：`claimed.insert(name)` 这一步**在**空版本号/非法模块名两条检查**之后**才跑（826-847 先 `continue`，848 才 `claim`）——也就是说一条「目录/清单层面」的拒绝**不占用**这个名字，后面一条同名但合法的 catalogue 条目仍然会被正常处理、可能变成 `Mounted`。已有测试直接证明一个名字可以同时有一条 `Mounted` 报告和一条 `Refused` 报告（`a_duplicate_name_is_refused_without_touching_its_store`，`domain.rs:1473` 起，`reports[0]==Mounted`、`reports[1]==Refused`，后到的那条因为「名字已被占用」被拒，不是因为它自己有问题）。

**v2 → v3** 曾经改成「全部同名报告都 `Refused` 才挡」——**v3 → v4（Codex 第 3 轮 High 3，这条聚合规则本身还是会被绕过）**：「全部都 `Refused`」和「全部都 `Disabled`」两个聚合条件放在一起看，漏了一种组合——一个 `Disabled`（赢得了这个名字，真正在挡的那条）+ 一个 `Refused`（输给前者、纯粹因为「名字已被占用」才被拒，`why` 是那句固定的「another module already claims the name」，和它自己的清单对不对没有任何关系）。这个组合下：「全部 `Refused`」不成立（有一条 `Disabled`）→ 第一道门放行；「全部 `Disabled`」也不成立（有一条 `Refused`）→ 重扫也不触发。两道门都被绕过，那条真正赢得名字的 `Disabled` 报告从没被检查过。要在 `os_routes.rs` 这一层正确处理，需要先分清「谁是真正赢得这个名字的那条」和「谁只是陪跑的重名噪音」——但陪跑的噪音到底是「跟这次请求毫不相干的另一个坏条目」还是别的，从 `os_reports` 这个拍平的列表本身看不出来，除非把 `mount_all` 内部「谁先拿到 `claimed`」这件事也编码进某个字段（又是一次不小的结构改动）。

**改法：把重名场景整体划出这次的范围，不试图猜出「谁是真正的那条」**——**只有这个名字在 `os_reports` 里恰好只有一条报告时，两道门（读现成判定 / 现场重扫）才生效**；出现多于一条同名报告（这本身已经是一种需要运营介入的目录配置问题——两个 provider 抢同一个名字），两道门都不触碰，`enable` 维持今天「不管准入判定、直接放行」的既有行为——不是新的错误，也不是新的保护，就是保持现状，跟「本设计关不上的其它口子」记在同一类：要真正关上需要能区分「谁赢得了这个名字」，这需要先给 `mount_all`/`MountReport` 引入某种「有效认领者」的概念，是一次独立的、值得单独写语义说明的改动，不在这次范围内：

```rust
let same_name: Vec<&MountReport> = state.os_reports.iter().filter(|r| r.name == name).collect();
if update.enabled
    && let [only] = same_name.as_slice()
    && let MountOutcome::Refused(why) = &only.outcome
{
    return error_response_with_hint(
        StatusCode::CONFLICT,
        "admission_refused",
        why,
        &format!(
            "`agent24 os list` shows the reason; correct it, then {RESTART_DAEMON_INSTRUCTION}, \
             and retry enable once the module is re-admitted"
        ),
    );
}
```

（这里一定至少有一条同名报告——上面已经过了「名字未知 → 404」那道门，同一个 `state.os_reports` 集合；「未知名字」判的是「压根没有任何同名报告」，这里判的是「恰好一条，且是 `Refused`」，两条门互不冲突，顺序不变。）

**只挡 `enabled: true`**。`enabled: false`（disable）不受影响——把一个本来就没有路由的模块标成「配置上也是禁用的」不会造成任何新的坏状态，反而让 `os.json` 如实记录操作者的意图；挡住它没有 F4e 要解决的那个问题（「写了个永远生效不了的启用位」）。

**只挡「恰好一条同名报告，且是 `Refused`」，不挡 `Degraded`**。`Degraded`（比如构造/存储初始化失败，`mount_all` 真实产生这个变体的场景——**v4 → v5（Codex 第 4 轮 Low 1，示例订正）**：不是崩溃退避或 `PackageChanged`，那两个是 `Status`，报告本身仍是 `Mounted`，下面 v1→v2 Low 2 的措辞已经说对了，这里的举例之前一直没跟上）是「现在起不来，但重启/修好之后可能会好」——这类模块的 `enable` 今天就是有意义的（等下次 daemon 重启就有效，`os list` 的 `restart_required` 字段已经在说这件事），不在这次收窄的范围内动它。`Mounted`/`Disabled` 显然也不碰；多于一条同名报告的情形见上面的重名说明，两道门都不触碰。

**v1 → v2（Codex 第 1 轮 Low 2，`Refused` 与 `Degraded` 的区分说法改正）**：v1 把 `Refused` 说成「永久性准入拒绝」——不准确，很多 `Refused` 原因（比如清单打错一个字段）改好就没了，**不是永久的**，只是「在这次启动的判定下没有路由」；真正的区分不是「永久 vs 暂时」，是「这次启动的准入判定就没给它路由」（`Refused`）vs「准入给了名字和路由，运行时状态不好」（`Degraded` 挂着一个 503 的命名空间，`Mounted`+ 运行时的 `PackageChanged`/退避同理）。全文「永久」相关措辞按此改写；`os_routes.rs:159` 附近如果有类似的旧注释，实现时一并核实要不要同步改（不在本设计强制范围，只是顺带一提）。

**状态码选 409 `Conflict`，理由收窄到真正贴切的先例**：v1 引了 `approvals.rs:117` 和 `runs.rs:29/31` 两处一起当依据——**v1 → v2（Codex 第 1 轮 Low 1）**：`runs.rs` 那处只是 `StoreError::Conflict` 的通用错误翻译，本身不能证明「这一类冲突就该用 409」；真正结构类似的是 `approvals.rs:117` 的 `approval_already_resolved`（一个语法合法的请求，撞上资源当前状态不允许这个操作）和 `StoreError::Transition`（状态迁移不合法）——`enable` 请求语法完全合法，问题是「这个名字现在的准入状态跟『启用』这个意图冲突」，是同一类「合法请求 vs 当前状态」的冲突，不是 400（请求本身有错）也不是 500（服务器出了意外）。不新造一个 422 的先例。

### Disabled 的进程外模块——`enable` 时现场重扫（v2 → v3，Codex 第 2 轮 High 1，新增小节；v3 → v4，Codex 第 3 轮全面订正机制本身）

上面那道门挡不住「当前 `Disabled`、清单其实对不上」这种情形——这类模块在 `os_reports` 里只有 `Disabled`，没有任何 `Refused` 报告可看。要关上它，需要在 `enable` 这一刻、针对**进程外**模块，现场重新核对一次清单——**v4 把 v3 的机制换成三处修正之后的版本**：

1. **`AppState` 新增两个字段，不是一个**：
   - `packages_root: Arc<PathBuf>`（照抄 `os_reports` 现在的存法）。
   - `package_dirs: Arc<HashMap<String, PathBuf>>`——**v3 → v4（Codex 第 3 轮 High 2）先是一个 `HashSet<String>`，v4 → v5（Codex 第 4 轮 Medium 1）改成 `HashMap<String, PathBuf>`**：光记「这个名字当初是不是 package」够用来做「按失败关闭」那个判定，但不够用来把 `scan.refused` 里的那条错误跟这个名字对上——`discovery::Refused`（`discovery.rs:64`）只有 `dir`/`why`，没有解析出来的名字（清单本身解析失败，名字可能压根拿不到），没法靠名字去 `scan.refused` 里找。改成记「名字 → 当初那个 `Package.dir`」，重扫阶段按**目录**而不是名字去匹配 `scan.refused`/`scan.found` 的条目——这样才能把「这个具体的包目录，现在解析失败/找不到了」的原因准确接回来。跟 `package_names` 一样，从 `catalogue: &[Installed]` 里 `Build::Package(package)` 一行推出来（`.filter_map(|e| match &e.build { Build::Package(p) => Some((e.name.clone(), p.dir.clone())), _ => None })`），不改 `MountReport` 的形状。
   - **v3 → v4（Codex 第 3 轮 Low 1）→ v4 → v5（Codex 第 4 轮 Medium 2）→ v5 → v6（Codex 第 5 轮 Medium 1，接线顺序第二次订正）**：v5 说「`packages_root` 在 `server.rs:1021` 已经算出来，比 `AppState::new` 调用点更早」——**还是错的**，再核实一次真实顺序：`AppState::new`（`server.rs:924`）在最前面，`state_dir`（`981`）、`packages_root`（`1021`）都在它**之后**才算。真正要做的改动是**把 `state_dir`/`packages_root` 这两步的计算从现在的位置挪到 `AppState::new` 调用**之前**，通过 `AppDeps`（或等价的构造参数）传进去**——这是一次实打实的顺序调整，不是「反正已经早，直接传就行」。`catalogue`/`package_dirs` 不需要跟着挪，仍然按 `os_reports` 现在的模式：`AppState::new` 里给 `package_dirs` 一个空 `HashMap` 占位，等 `catalogue` 建完、紧挨着 `os_reports` 被重新赋值的那个点（`server.rs:1285` 附近）用同一个已经在作用域里的 `catalogue` 变量算出真值，**重新赋值**（不是新建一个 `AppState`）。测试构造 `AppState` 时要求显式传一个临时目录当 `packages_root`（不给空路径当默认值），`package_dirs` 默认为空、需要重扫场景的测试显式设置。
2. **`enable` 请求，且这个名字恰好一条同名报告、`outcome` 是 `Disabled`、且 `name` 在 `package_dirs` 的键里**（三个条件缺一都不触发重扫）：
   a. 先核对 `packages_root` 本身没被篡改——`agent24_os_packages::check_packages_root(&state.packages_root)`（真实存在的函数，`agent24-os-packages/src/lib.rs:158`，不创建目录，只核对符号链接/目录类型/属主/群组和全局写权限——`ensure_packages_root`，`lib.rs:129`，是它加了「不存在就创建」的外壳；`scan()` 自己只管 `read_dir`，不做这层核对，一个被替换成符号链接或者变成全局可写的 `packages_root`，重扫能读到一份「凑巧对得上」的清单，但下次真正启动会在这一步就拒绝整个目录、什么都不装）。**v5 → v6（Codex 第 5 轮 Low 1，这一步同时兜住「整个根目录不见了/读不了」，跟单个包子目录不见了要分开说**：`check_packages_root` 在根目录本身缺失、不可读、类型不对的时候就会先在这一步失败——不会走到 b 步、更不会落进「两边都没有 `dir` 匹配」那支；`scan()` 自己在根目录 `read_dir` 失败时会往 `scan.refused` 塞一条 `dir` 是**根目录本身**的记录（`discovery.rs:92` 附近），这条不代表任何一个具体的包目录，跟按包目录匹配的逻辑不是一回事，不能被误当成「这个包的目录不见了」）核对失败**按失败关闭**处理，`message`/`hint` 直接说明「安装它的这整个目录（`packages_root`）现在核对不过，看这次应答给出的具体原因；可能是权限变了或者被替换成了符号链接」（不再说「去 `agent24 os list` 看」——`os list` 不会显示这次现场核对出来的原因，指错地方）；
   b. 根目录本身核对通过之后，再调 `agent24_os_packages::discovery::scan(&state.packages_root)`。**v6 → v7（Codex 第 6 轮 Low 1）**：`check_packages_root` 只核对符号链接/类型/属主/写权限这几项元数据，不实际尝试 `read_dir`——一个属主对、权限位却是 `0300`（只写不可读）之类的目录能通过 a 步的核对，但 `scan()` 自己在 `read_dir` 失败时会往 `scan.refused` 塞一条 `dir` 就是 `packages_root` 本身的记录（不是任何具体包的目录）。**扫描结果先看这一条，不先按包目录匹配**：
      - **`scan.refused` 里有一条 `dir == state.packages_root` 本身**（根目录元数据没问题，但读不了）→ 按跟 a 步一样的「根目录本身核对不过」处理，`message`/`hint` 说明整个安装目录读不了、给出这条 `Refused` 自带的原因，**不**往下走到按包目录匹配那一步（不然会把「根目录读不了」误报成「这个包不见了」）。
      - 否则，用第 1 步记的 `dir`（不是名字）去 `scan.found`/`scan.refused` 里找**这个包自己的目录**：
      - **`scan.refused` 里有一条 `dir` 匹配**（这个包的目录还在，清单解析失败）→ 挡，`message` 用这条 `Refused` 自己带的 `why`。
      - **两边都没有这个包自己的 `dir` 匹配**（这个包的子目录被卸载/挪走了——根目录本身是好的，a 步已经通过）→ **按失败关闭**，`message`/`hint` 说明「安装它的包目录已经不在磁盘上原来的位置了，确认是不是被卸载或者重装到了别处」。
      - **`scan.found` 里有一条 `dir` 匹配**——**检查两条**（**v5 → v6（Codex 第 5 轮 High 1，补回名字检查）**：v5 删掉了全部「记住的」catalogue 比对，但删过头了——目录匹配只能证明「这是当初那个包目录」，不能证明「这个目录里的清单现在还叫这个名字」；如果这段时间清单被改名了（比如运营手动改了 `name` 字段，交付方式还是合法的进程外），按目录能找到它、交付方式检查也能通过，`enable {旧名字}` 就会被放行、成功落盘——但下次重启 catalogue 只会有新名字，旧名字这个启用位永远生效不了，正是本设计要消灭的坏状态。补回**名字**这一条检查（**不是版本**——名字是 `os.json` 的配置键，直接决定这次 `enable` 请求对不对得上这个目录；版本不是配置键，且理由见「决策记录」，比它没有意义）：
        - `manifest.name() != name`（请求的名字，也是 `package_dirs` 这个键本身）→ 挡，`message` 说明「这个目录里的清单现在声明的名字是 {manifest.name()}，跟 {name} 不一致，可能是清单被改名了」；
        - 名字对得上，但 `manifest.spawn().filter(|_| !manifest.is_mountable_in_process()).is_none()`（清单自称编译进内核的交付方式，却是以 package 形式装在磁盘上）→ 挡，`message` 用 `mount_package`（`domain.rs:1151` 附近）同款的措辞；
        - 两条都过 → 放行。
   c. `scan`（含 a 步的目录核对）**必须包进 `tokio::task::spawn_blocking`**：这是同步文件系统 I/O——`read_dir`/逐个读文件/算哈希/解析 YAML，直接摆在 axum 的 async handler 里跑会占住执行器线程；仓库里 `patch_os` 自己写配置（`os_routes.rs:438` 附近）已经走 `spawn_blocking`，这里跟着同一个模式，闭包只包 a/b 两步（核对+扫描），把 `JoinError` 转成 `500 internal`；应答构造、拿锁、写配置留在闭包外的 async 部分，不要连它们一起塞进阻塞任务。
3. **挡住时的 `code` 跟第一道门一样是 `admission_refused`，`hint` 不一样**：第一道门挡的时候，`os_reports` 里那条 `Refused` 报告本身就记着原因，所以 hint 能说「`agent24 os list` 显示了原因」；这里不一样——这条 `Disabled` 报告**依然只会显示** `Disabled`，刚查出来的真实原因只存在于**这次 PATCH 的应答里**，写「去 `os list` 看」是在指错地方（a/b 两个失败分支同理，见上面各自的措辞）。而且顺序也反了：`os.json` 这次**没有**被写，所以「先修好、再重启」这个流程本身就不成立——重启之后它照样只会显示 `Disabled`（准入判定压根没跑到清单那一步），不会自动变成 `Refused` 让人看出问题。正确的流程是「先修好磁盘上的东西 → 重试这次 `enable`（这次重扫会通过、真的落盘）→ 再重启一次 daemon 让它生效」。b 步「交付方式自相矛盾」这条分支专用的 `hint`：`"the reason is above, not in \`agent24 os list\` (this module is still recorded as disabled there); fix it on disk, then retry enable — once it passes, {RESTART_DAEMON_INSTRUCTION}"`。

**为什么不对编译进内核的模块做等价的事**：它的「清单」不是一个可以单独读的文件，来自调用 `build()` 闭包的副作用（`domain.rs:935` 附近）——要核对就得真的构造它，而"disabled 不构造"这条线存在的唯一原因就是不让一个坏构造器在 `enable`/`disable` 这种轻量操作里拖垮 daemon（`identity_admission_beats_disabled_but_manifest_admission_cannot` 测试的原话）。检查不了就是检查不了，记成独立跟进项（见决策记录）。

**并发/一致性——明说是尽力而为，不是硬保证**（**v3 → v4（Codex 第 3 轮 Medium 4）**：v3 说「不改变现有的并发保证」——说得太满。重扫和最终写 `os.json` 之间有一段没有锁保护的窗口：包完全可能在重扫通过**之后**、写盘**之前**被卸载或替换（无论是这台机器上的另一个 `agent24 os uninstall` 调用，还是运营脚本直接动了磁盘），`os_control` 锁只序列化 `patch_os` 自己的并发写，不协调安装/卸载。这不是本设计要解决的问题——**明确写成一个已知的、可接受的尽力而为限制**：重扫给出的是「这一刻看起来能行」，不是「从这一刻到重启之间永远能行」的保证，跟 B 段一贯的态度（第一轮设计审查已经确立的「尽力而为，不是正确性边界」）是同一类表态，不是新的例外。真要把这个窗口关上，需要一个横跨安装/卸载/准入检查的公共锁，是独立的、值得单独评估要不要做的工作。）

### 协议文档（v2 → v3，Codex 第 2 轮 Low 2，新增小节）

`ErrorBody.code` 在 `agent24-protocol/src/types.rs:176` 有一份文档列出目前已知的取值（开放式枚举，字符串类型）。**v3 → v4（Codex 第 3 轮 Low 2，事实订正）**：v3 说这份清单「FU-64/ERR-1 加新 code 时也同步更新过」——查了一下，`module_panicked`/`module_killed` 今天并不在这份清单里，它本来就已经是不完整/没跟上的状态，不是这次才发现的问题。改成如实说：`admission_refused` 加进去的同时，顺手把已知漏掉的几个也补全，这是在**修一份已经过时的清单**，不是「跟随一贯维护得很好的先例」——措辞上的差别，但对写码的人来说，意味着不能只加自己这一个 code 就完事，得先看一眼这份清单现在到底缺了哪些。

**不做**：不新增一条「立即重新核对准入」的**通用**接口/命令（比如 `POST /api/v1/os/{name}/recheck`）——这次重扫是 `enable` 这一个动作内部的实现细节，不对外暴露成独立可调用的东西；对照 FU-61 的 `discovery::recheck`，那是给*已经在跑*的模块在*每次重启前*核对，用途、触发时机都不一样，这里不复用也不模仿那个公开形状。`mount_all` 本身仍然只在 daemon 启动时跑一次，这次只是给 `enable` 单开一条窄的、只读的重扫路径，不改变「准入判定只在启动时算」这个架构假设本身。

### 怎么可测——不碰真实 `$HOME`

**v1 → v2（Codex 第 1 轮 Medium 5，新增小节）**：`patch_os` 今天的成功写入路径（`os_routes.rs:607`，`config_path()` → `agent24_protocol::state_file::state_dir()`）读的是进程级的 `$HOME`，没有任何注入点；v1 的判据 1/3 都要求断言「成功落盘」/「没有落盘」，但照今天的函数签名写测试，要么真的改动跑测试那台机器的 `~/.agent24/os.json`（不安全，Rust 测试并发跑、`$HOME` 是进程级全局状态，改一次影响同进程里其它并发测试），要么没法验证「真的落盘了/真的没落盘」这件事本身。改法：把 `patch_os` 的主体拆成一个不碰全局的内部函数，接收路径作为参数，公开的 axum handler只做「调 `config_path()` 拿到真实路径，再调内部函数」这一层转发：

```rust
pub async fn patch_os(state: State<AppState>, name: Path<String>, req: Request<Body>) -> Response {
    patch_os_at(state, name, req, crate::os_config::config_path()).await
}

async fn patch_os_at(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request<Body>,
    path: Option<PathBuf>,
) -> Response {
    // 原有逻辑不变，只是原来直接调 config_path() 的地方改成用这个参数
}
```

测试调 `patch_os_at(..., Some(tmp_path))`，生产路径调 `patch_os` 不变，两边共享同一段准入/写入逻辑，不会出现「测试测的是一份简化过的逻辑，和生产实际跑的不是同一段代码」这种常见坑。这个拆分只影响这一个 handler 内部，不改路由签名、不改对外契约。

**v2 → v3（Codex 第 2 轮 Medium 1，`patch_os_at` 拆得不彻底）**：v2 只拆了 `patch_os` 自己读 `config_path()` 的那一次调用——但成功路径最后调的 `render(&state)`（`os_routes.rs:683`）**自己也**独立调了一次全局 `config_path()`（`os_routes.rs:230`）去读配置、拼渲染结果。测试用临时路径写完之后，`render` 还是会去读机器上真实的 `~/.agent24/os.json`——判据 2/3/6（断言成功落盘之后返回的视图）和判据 4（拿 GET 的结果跟 PATCH 的错误消息比对）全都会读到不相关的真实文件，结果不确定。补一个 `render_at(state, path: Option<&Path>)`，`patch_os_at` 用同一个注入的 `path` 调它，不再各自独立取一次全局路径；`list_os`（`GET /api/v1/os`）同理需要一个可注入路径的测试入口，判据 4 才能真的对比到同一份临时配置文件算出来的两个视图，不是两份不同来源的数据凑巧看着像。

## 判据（带正对照）

1. **主判据**：用 `os_routes.rs` 里既有的 `report(name, outcome)` 测试构造器（`os_routes.rs:1055`）直接造一条 `MountReport{name: "refused-mod", outcome: MountOutcome::Refused("catalogue version mismatch".into())}` 装进测试用的 `AppState.os_reports`（不需要真跑 `discovery`/`mount_all`——`report()` 本来就是为绕开这套机器而存在的单元构造器，**v1 → v2（Codex 第 1 轮 Medium 3）**：v1 拿「清单里没有 `spawn`」当例子不成立，那类清单连 catalogue 都进不去，不会产生任何 `MountReport`，见上面「问题」段新加的分层表格；改用一个真实会产生 `MountOutcome::Refused` 的构造，比如`report()`直接给的合成值最简单，不需要影射某个具体拒绝原因）。`PATCH /api/v1/os/refused-mod` 带 `enabled: true`，回 `409`，`error.code == "admission_refused"`，`error.message == "catalogue version mismatch"`（逐字等于构造时给的 `why`，不是 `os_routes.rs:31` 那种带模块名前缀的拼接文本——**v1 → v2（Codex 第 1 轮 Medium 1）**：v1 的响应代码片段用 `format!("{name:?} was refused at mount: {why}")` 却又在这条判据里要求跟 `os list` 的 `detail`「逐字相同」，`os list` 的 `detail` 就是裸 `why`，两者自相矛盾——已在「设计」段改成直接用 `why` 本身，不包一层前缀），`error.hint` 非空且**逐字包含** `RESTART_DAEMON_INSTRUCTION` 常量的完整文本（不是「提到重启」这种宽松断言——直接 `assert!(hint.contains(RESTART_DAEMON_INSTRUCTION))`，防止实现时手写一句新的、不托管感知的重启建议）。用 v2 新增的 `patch_os_at(..., Some(tmp_path))` 测试入口，断言 `tmp_path` 指向的文件请求前后字节完全一致（不只是「没有这个名字的新 entry」，连同目录里别的既有 entry 也不该被动到）。正对照：去掉这道门——同一请求变成 200 且落盘 `enabled: true`。
2. **不误伤 `Degraded`**：`report()` 构造一条 `MountOutcome::Degraded("store open failed: ...".into())`（**v1 → v2（Codex 第 1 轮 Medium 3）**：v1 拿 `PackageChanged`/退避中当例子不成立——那些是 `supervisor::Status` 上的值，报告本身的 `outcome` 仍然是 `Mounted`，`view()` 只是把它们也渲染成字符串 `"degraded"`，但它们不是 `MountOutcome::Degraded`；改用 `mount_all` 真实会产生 `MountOutcome::Degraded` 的场景，比如资源/存储初始化失败——找一个已有产生 `Degraded` 的测试作参照，或直接用 `report()` 手工构造），同样的 `enabled: true` 请求，走既有路径成功落盘（今天的行为不变）。正对照（**v3 → v4 随 High 3 改写**）：把门的匹配模式从「`[only]` 且是 `Refused`」错误地放宽成「`[only]` 且非 `Mounted`」（也就是把 `Degraded` 一起当成挡的条件）——这条测试变红。
3. **`enabled: false` 不受影响**：同一个 `Refused` 的模块，`enabled: false` 请求正常成功落盘（今天的行为不变，`os.json` 如实记下「用户也主动禁用了」）。正对照：把门的条件从 `update.enabled &&` 误删——这条测试变红。
4. **`message` 与 `os list` 一致性**：同一个 `Refused` 模块，分别打 `GET /api/v1/os` 和被拒的 `PATCH`，断言两处的原因文案逐字相同（防止两处各写一份、后续漂移）。
5. **未知名字仍是 404，不是 409**：一个压根不在 `os_reports` 里的名字，`enabled: true`，仍然是既有的 `404 not_found`，不会被误判成 `Refused`（两道门的顺序：未知名字的检查在前，本设计不改这个顺序）。**v2 → v3（Codex 第 2 轮 Low 1，措辞限定）**：「未知名字」这个判据本身只在**没有任何同名报告**时成立——如果这个名字被一个合法的 catalogue 条目占着（哪怕另一个同名条目曾经因为空版本号之类的原因被拒），`os_reports` 里就有它的报告，判据要测的是这种真正查无此名的情形，不要和判据 6/7 的重名场景混着写断言。
6. **重名——多于一条时两道门都不触碰**（**v1 → v2（Codex 第 1 轮 Medium 2）新增；v3 → v4（Codex 第 3 轮 High 3）改成跳过而不是聚合判定**）：两条同名 `MountReport`，一条 `Mounted`、一条 `Refused`（照抄 `a_duplicate_name_is_refused_without_touching_its_store` 的构造，`domain.rs:1473`），`enabled: true` 请求成功落盘——不是因为门判断出「有一条能用」，是因为**恰好一条**这个前提不成立，门整体不触碰，落到今天的既有行为。正对照：把 `[only]` 这种「恰好一条」的匹配错误地放宽成「随便匹配到一条就判定」——如果 `os_reports` 里 `Refused` 那条排在 `Mounted` 前面，这条测试会变红（重现 v1/v2 的假阳性）。
7. **重名——`Disabled`+`Refused` 组合也不触碰**（**v3 → v4（Codex 第 3 轮 High 3）新增，专门钉住这一轮抓到的绕过路径**）：两条同名 `MountReport`，一条 `Disabled`（真正赢得这个名字的那条）、一条 `Refused`（`why` 是「另一个模块已经占了这个名字」，跟它自己的清单无关），`enabled: true` 请求成功落盘——第一道门因为「不是恰好一条」不触碰，重扫也因为「不是恰好一条 `Disabled`」不触碰，两道门都不判定，行为等同今天。正对照：把重扫的触发条件从「恰好一条且是 `Disabled`」放宽成「存在一条 `Disabled`」——这条测试会变红（重现第 3 轮 High 3 描述的绕过）。
8. **写入前置校验的顺序**（**v1 → v2（Codex 第 1 轮 Medium 5），新增**）：一个不存在的名字带一个语法错误的请求体（比如非法 JSON），仍然先走「未知名字 → 404」，不会因为准入门插在body 解析之后就意外提前触发别的分支；一个 `Refused` 名字带语法错误的请求体，回既有的 `400 invalid_request`（body 解析失败在准入门之前，见「设计」段的插入点），不是 `409`——确认新加的门没有打乱既有的检查顺序。
9. **`Disabled` 的进程外模块，交付方式自相矛盾——重扫挡住**（**v2 → v3（Codex 第 2 轮 High 1）新增；v3 → v4（Codex 第 3 轮 High 1）换掉判定内容，不再比版本；v4 → v5（Codex 第 4 轮 Medium 3）修掉一个会让这条测试假绿的漏洞**，本设计最重要的一条判据）：在临时 `packages_root` 里放一份磁盘上的清单，**`manifest.name()` 必须等于 `package_dirs` 记的那个名字**（不能随便写——如果清单里的名字跟请求的名字对不上，会被判据 9b 那条**新加的名字检查**挡住，不是这条测试要考的），`version` 可以随便写（这条确实不检查它），但自称 `is_mountable_in_process() == true`（编译进内核的交付方式）；`AppState.package_dirs` 里这个名字对应的目录就是这份清单所在的目录，`os_reports` 里这个名字恰好一条 `Disabled` 报告；`enable` 请求回 `409`，`error.code == "admission_refused"`，`error.message` 包含 `mount_package` 同款的交付方式矛盾措辞，`error.hint` 是判据 12 描述的那条专用文案（不是共享的重启文案）。正对照：去掉交付方式这一步检查（保留目录匹配和名字检查）——同一请求成功落盘（复现「Disabled → enable → 下次重启才发现被拒」）。
9b. **同一目录、清单被改名、交付方式仍然合法——照样要挡**（**v5 → v6（Codex 第 5 轮 High 1）新增，钉住这一轮抓到的绕过路径**）：`package_dirs["alpha"] = D`（`Disabled` 报告），运行期间 `D` 目录里的清单被改成声明 `name: "beta"`（交付方式还是合法的进程外模式）；`enable alpha` 请求回 `409`，`error.message` 提到清单现在声明的名字跟请求的名字不一致。正对照：删掉名字检查、只留目录匹配 + 交付方式检查——这条测试变红（复现「按目录能找到、交付方式也没问题，但 enable 的这个名字下次重启压根不会出现在 catalogue 里」）。
10. **`Disabled` 的进程外模块，清单其实没问题——放行**：同样构造一个 `Disabled` 报告，`package_dirs` 含这个名字对应的目录，磁盘上该目录里的清单名字对得上、交付方式自洽（进程外、带合法 `spawn`），`enable` 成功落盘（这是最常见的正常 enable 场景，必须不能被新加的重扫误伤）。正对照：把重扫的判断条件写反——这条测试变红。
11. **`Disabled` 的编译进内核模块——不重扫，照旧放行**：一个 `Disabled` 报告，`name` **不在** `AppState.package_dirs` 的键里（模拟编译进内核）；`enable` 成功落盘，且不触发任何重扫（不需要真的摆一个 `packages_root` 出来）。正对照：把「不在 `package_dirs` 里就放行」错误地改成「扫描目录里找不到就放行」（v3 的旧逻辑）——这条区分了「压根不是 package」和「曾经是 package、现在不见了」两种「找不到」，不能用同一个信号处理，判据 13 专门测后一种。
12. **重扫挡住时的 hint 是分支专用的，不是共享文案**：判据 9 挡住的应答里，`error.hint` 提到「原因在这次应答里，不在 `agent24 os list`（那里仍然显示 `disabled`）」且没有说「重启」是第一步——先修、先重试 `enable`、这次会真的落盘，最后才重启。正对照：如果重扫挡住时复用第一道门的共享 hint（`agent24 os list` 显示了原因），这条测试变红——`os list` 在这个分支下确实不会显示新原因，断言字面文本会不匹配。
13. **这个包自己的子目录彻底找不到，但 `packages_root` 本身是好的——按失败关闭**（**v5 → v6（Codex 第 5 轮 Low 1）跟判据 15 分开，不再混用**）：`packages_root` 本身核对通过，`package_dirs` 记的这个包的子目录却已经不存在了（模拟单独卸载了这一个包，其它包和根目录都没事）；`enable` 请求回 `409`，`error.message`/`hint` 说明「这个包目录不在原来的位置了」，不提「去 `os list` 看」。正对照：把「这个包的目录找不到」当成「放行」处理——这条测试变红。
14. **目录还在但清单解析不了——按目录匹配到 `scan.refused`、按失败关闭**：`package_dirs` 记的目录还在，但里面的清单文件本身是非法 YAML（这份 `Refused` 记录本身可能压根拿不到一个可信的名字，所以匹配靠 `dir` 相等，不靠名字）；`enable` 请求回 `409`，`error.message` 用这条 `scan.refused` 记录自带的错误原因。正对照：改成按名字去 `scan.refused` 里找（而不是按目录）——如果一份解析失败的清单确实拿不到名字，这条测试会因为「找不到匹配」掉进判据 13 的分支而不是判据 14 期待的分支，断言的 `message` 内容对不上，变红。
15. **`packages_root` 整个根目录本身核对不过/读不了——按失败关闭，且不跟判据 13 混用**（**v5 → v6（Codex 第 5 轮 Low 1）明确这条测的是根目录层面，不是单个包子目录**）：两个子场景都要测——
    - **15a（`check_packages_root` 本身就拒**）：把临时 `packages_root` 设成全局可写（或者一个指向别处的符号链接），触发这个名字的重扫路径；`enable` 请求回 `409`，`error.message`/`hint` 明确说的是**整个安装目录**核对不过，不提「去 `os list` 看」。正对照：跳过 `check_packages_root` 直接扫描——这条测试变红。
    - **15b（`check_packages_root` 通过、但 `read_dir` 本身失败——v6 → v7（Codex 第 6 轮 Low 1）新增**）：`packages_root` 的属主/权限位/类型都能通过 `check_packages_root` 的元数据核对（比如 `0300`，只写不可读），但 `scan()` 自己 `read_dir` 会失败，落进 `scan.refused { dir: packages_root 本身 }`；`enable` 请求同样回 `409`、同样是「整个安装目录」的措辞，**不是**「这个包不见了」。正对照：去掉「先看 `scan.refused` 里有没有 `dir == packages_root` 本身」这一步、直接按包目录匹配——这条测试会掉进「这个包的子目录不见了」（判据 13）那支，`message` 文本对不上，变红；这条同时也是判据 13/14 的负例：确认它们的按目录匹配逻辑**不会**把这条根目录级别的 `scan.refused` 记录误认成任何一个包自己的记录。
16. **`scan` 真的在阻塞线程池里跑**：不容易直接断言「在哪个线程跑」，退而求其次——断言重扫路径的整个请求处理不会阻塞同一 executor 上的其它并发任务（比如同时发一个不相关命名空间的健康检查请求，断言它的延迟不受重扫影响），或者至少在实现里留一个可以静态审查的标记（`spawn_blocking` 调用本身只包 a/b 两步，不含响应构造/加锁/写盘），CI 跑得到、人也审得到。

## 不改的东西

- `MountOutcome` 的变体或判定逻辑本身——不新增准入检查（spawn 目标是否存在、依赖是否满足两类明确排除在外，见「决策记录」）；进程外 `Disabled` 模块的重扫复用 `mount_package` 已有检查里的**交付方式自洽性**这一条，再加一条**清单现在的名字是否等于请求的名字**（**v5 → v6（Codex 第 5 轮 High 1）**补回——不是 `mount_package` 原有检查的一部分，是这次现场重扫特有的，因为目录匹配证明不了名字没被改过），不再比对 catalogue 记的旧版本；这两条加起来仍然不是新发明的判定标准，都是「这个目录里的东西现在到底是什么」这件事本身的直接推论。
- `MountReport` 的形状——不加字段（provenance 信息走单独的 `AppState.package_dirs`，不是塞进这个被大量既有测试构造的结构）。
- `mount_all`/`mount_package` 本身——不改它们，`enable` 时的重扫是在 `os_routes.rs` 里独立照抄同款检查，不是把这两个函数抽出来复用（它们绑着 `Router`/`ProcessHost` 等一堆跟「只读检查」无关的东西，硬复用比照抄一行检查逻辑更麻烦，也更容易引入新的耦合）。
- `mount_all` 只在启动时跑一次这个架构假设——不引入后台监听或按需重算*整个准入判定*；`enable` 时的重扫只针对**这一个名字**、只在**这一次请求**里发生，不是把 `mount_all` 变成随时可以重跑的东西。
- 编译进内核模块的 `enable` 行为——这条口子明确关不上，见决策记录。
- 多于一条同名报告的 `enable` 行为——两道门都不触碰，维持今天的既有行为，见判据 6/7。
- `Mounted`/`Degraded` 两个分支的 `enable` 行为。
- `enabled: false`（disable）路径。
- `os list`/`view()` 的既有渲染逻辑——`message` 只是读它，不改它。
- `patch_os` 的公开签名/路由挂法——只在内部拆出带路径参数的私有函数。
- 安装/卸载/`agent24 os uninstall` 的既有逻辑——不引入跨这些操作的公共锁，重扫和写盘之间的窗口明说是尽力而为，见「设计」段。

## v1 → v2 改动（Codex 第 1 轮：0 Critical、1 High、5 Medium、2 Low，全部采纳）

- **High 1**：「问题」段重写——SPEC 点名的四类拒绝原因分布在三个不同的层，新增分层表格；「决策记录」明说本设计只关上「版本不符」「清单自相矛盾」两类，「清单解析失败」这类已经靠既有的未知名字 404 关上（不需要本设计动手），「spawn 目标不存在」「缺依赖」两类**关不上**、需要记成独立跟进项，不能让 `DONE` 掩盖它们仍然开着。
- **Medium 1**：响应 `message` 的构造代码片段和判据要求的「逐字相同」自相矛盾——改成直接用 `why`，不包前缀。
- **Medium 2**：重名场景下 `.find()` 取第一条同名报告会产生假阳性（一条无关的空版本号拒绝可能排在一条本该放行的条目前面，且已有测试证明这种「一好一坏」的组合真实存在）——改成「全部同名报告都 `Refused` 才挡」，挡住时用第一条 `Refused` 的 `why`；新增判据 6/7 覆盖两种重名组合。
- **Medium 3**：判据 1/2 举的例子在代码结构下根本无法构造（`Refused` 例子是解析期就拒、不会有 `MountReport`；`Degraded` 例子其实是 `Mounted` + `supervisor::Status`，不是 `MountOutcome::Degraded`）——改用仓库已有的 `report()` 测试构造器直接合成需要的 `MountOutcome`，不依赖跑不通的真实场景。
- **Medium 4**：hint 引用了一个不存在的常量名（`RESTART_DAEMON_INSTRUCTION_WITH_ADMISSION_NOTE`）——改成直接复用已有的 `RESTART_DAEMON_INSTRUCTION`，拼进一句本地组合的话；判据 1 从「提到重启」这种宽松断言改成「逐字包含这个常量」。
- **Medium 5**：`patch_os` 的成功写入路径依赖进程级 `$HOME`，判据 1/3 要求断言的「落盘/不落盘」在今天的函数签名下没法安全测试——拆出一个接收路径参数的内部函数 `patch_os_at`，公开 handler 只做转发；新增判据 8 覆盖门和既有检查的顺序没被打乱。
- **Low 1**：409 的先例收窄成真正结构类似的 `approval_already_resolved`/`StoreError::Transition`，不再笼统地引用通用的 `StoreError::Conflict` 翻译当依据。
- **Low 2**：「永久性准入拒绝」的说法不准确（很多 `Refused` 原因改好就没了）——改成「这次启动的准入判定没给它路由」，和 `Degraded`（「准入给了路由，运行时状态不好」）的区分不再是「永久 vs 暂时」。
- （Critical：本轮无。）

## v2 → v3 改动（Codex 第 2 轮：0 Critical、2 High、2 Medium、2 Low，全部采纳；范围扩大经用户 2026-09-16 裁决）

- **High 1**（本轮最重要）：v2 的门只读 `os_reports` 里现成的判定，挡不住「当前 `Disabled`、清单其实对不上」——这是 F4e 最常见的触发方式（`mount_all` 故意让 `Disabled` 短路在清单相关检查之前，一个禁用的模块从没被真正核对过）。用户裁决扩大范围：新增「`enable` 时对进程外 `Disabled` 模块现场重扫清单」的小节，复用 `mount_package` 已有的两条纯检查（无副作用，只读文件解析 YAML），不碰构造/spawn；编译进内核的模块明确关不上（核对需要调用 `build()`，正是「disabled 不构造」这条安全线要挡的东西），记成独立跟进项。新增判据 9/10/11。
- **High 2**：「版本不符」被当成一种拒绝原因，实际是三种彼此无关的检查（清单 schema 层面、catalogue-vs-已装清单、握手协商区间）——分层表格拆成三行，明确只有 catalogue-vs-已装清单版本这一种是本设计关上的；「清单自相矛盾」的措辞也改正（不是「缺 spawn 字段」，是「清单自称的交付方式跟安装形态对不上」）。
- **Medium 1**：`patch_os_at` 只拆了写入路径，成功路径最后调的 `render()` 自己独立读了一次全局 `config_path()`——判据 2/3/4/6 的落盘断言仍然不确定。补 `render_at(state, path)`，`patch_os_at` 和 `list_os` 的测试入口都改用注入路径。
- **Medium 2**：随 High 2 一并修正——「清单非法（自相矛盾）」那格的具体机制描述改正。
- **Low 1**：「未知名字 → 404」这条判据补上「没有任何同名报告」的限定，避免跟重名判据的断言范围混淆。
- **Low 2**：新增「协议文档」小节，提醒 `admission_refused` 要同步进 `ErrorBody.code` 的已知值清单，跟 FU-64/ERR-1 当时同步协议制品是同一类机械步骤。

## v3 → v4 改动（Codex 第 3 轮：0 Critical、4 High、5 Medium、2 Low，全部采纳；重扫机制本身重写）

- **High 1**（最重要）：v3 拿重扫出来的清单去比对上次启动时记的 catalogue 版本——这个基准是错的，对一个正常发现的包，catalogue 身份本来就是从当时的清单直接派生，天然一致，拿旧记录当基准比对只会把「包被合法升级过」误判成「版本不符」。改成重扫**只检查交付方式自洽性**，不再比对任何记住的名字/版本；判据 9 换成用交付方式矛盾来触发。
- **High 2**：「扫描找不到清单 = 编译进内核」这个推断不成立——`MountReport` 不记录 provenance，找不到也可能是包被卸载/改名/清单现在解析不了/目录读不了。改成新增 `AppState.package_names`（一个从 catalogue 直接推导的名字集合，不碰 `MountReport` 的形状），用它分辨「当初是不是 package」；「是 package 但现在扫描不到」按失败关闭，不再放行。新增判据 11（订正）/13/14。
- **High 3**：「全部同名报告都 `Refused`/`Disabled`」这两个聚合条件放在一起看仍有绕过——一个赢得名字的 `Disabled` + 一个因为重名而被拒的 `Refused`，两道门都不成立。改成两道门都只在「恰好一条同名报告」时才触碰，多于一条一律跳过、维持既有行为，重名场景整体划出这次范围。判据 6/7 相应改写。
- **High 4**：重扫直接调 `scan()`，跳过了启动时会做的 `packages_root` 自身信任核对（属主/权限/符号链接）——一个被篡改的目录能让重扫通过、但下一次真启动会整个拒绝。补上 `check_packages_root` 这一步，失败按拒绝处理，不当成「编译进内核」。新增判据 15。
- **Medium 1**：`scan()` 是同步文件系统 I/O，不能直接摆在 axum 的 async handler 里跑——按仓库已有的 `spawn_blocking` 先例包起来。新增判据 16。
- **Medium 2**：判据 9/10/11 的旧版例子（版本比对、扫描找不到即放行）随 High 1/2 一起过时——全部重写以匹配订正后的机制。
- **Medium 3**：重扫挡住时复用了第一道门的共享 hint，但这个分支下 `os_reports` 里的报告依然只显示 `Disabled`，`os list` 看不到新原因，且流程顺序也反了（应该是先修、先重试 `enable`，而不是先重启）——写一条这个分支专用的 hint，新增判据 12。
- **Medium 4**：「重扫不改变现有并发保证」这句话说得太满——重扫和写盘之间有一段没有锁保护的窗口，`os_control` 不协调外部的安装/卸载。改成明说这是尽力而为、不是硬保证，跟 B 段一贯的措辞对齐。
- **Low 1**：`packages_root` 今天在 `AppState` 构造**之后**才算出来——顺序要倒过来，在构造之前算好、通过 `AppDeps` 传入，跟 `os_reports` 现在的顺序一致；测试要求显式注入临时目录，不给空路径当默认值。
- **Low 2**：`ErrorBody.code` 的已知值清单其实已经漏了 `module_panicked`/`module_killed`，不是「FU-64/ERR-1 时代维护得很好、这次跟着做」——改成如实说这是在修一份已经过时的清单。
- （Critical：本轮无。）

## v4 → v5 改动（Codex 第 4 轮：0 Critical、0 High、3 Medium、2 Low，全部采纳；机制核心三处判定复核为正确）

- **Medium 1**：`AppState.package_names: HashSet<String>` 能做「按失败关闭」的判定，但没法把 `scan.refused` 里的一条错误跟具体名字对上（`discovery::Refused` 只有 `dir`/`why`，没有解析出来的名字）。改成 `package_dirs: HashMap<String, PathBuf>`，重扫按**目录**匹配 `scan.found`/`scan.refused`，不按名字；判据 14 改成按目录匹配，并补上「解析失败的清单本就可能拿不到可信名字，所以不能靠名字匹配」这条理由。
- **Medium 2**：v4 说「`packages_root` 在 `AppState` 构造前算好、传进去」——核实 `server.rs` 真实顺序之后发现 `catalogue` 直到 `AppState::new` 调用之后才建完，「构造前传 catalogue 派生值」这句话在当前代码结构下不成立。改成跟 `os_reports`完全一样的接线方式：`packages_root` 构造时传入（它确实算得比 `AppState::new` 早），`package_dirs` 先给空占位、等 `catalogue` 建完后在赋值 `os_reports` 的同一个点重新赋值，不是新建一个 state。
- **Medium 3**：判据 9 允许清单的 `name` 随便写——如果清单名字跟报告名字对不上，请求会被「按目录找不到」那一支挡住，删掉交付方式检查本身这条测试也照样是红的，测不出真正要测的东西。改成要求清单的 `manifest.name()` 必须等于 `package_dirs` 记的名字，只留 `version` 可以随便写。
- **Low 1**：三处遗留的不准确措辞——开场段落仍把「缺依赖」当成真实发生的拒绝原因（跟下面表格的核实结论矛盾）；决策记录的收尾句暗示版本比对在「读现成判定」和「现场重扫」两条路径都会做（v4 已经明确重扫不比版本）；`Degraded` 的举例还在用崩溃退避/`PackageChanged`（那两个其实是 `Mounted` + 运行时 `Status`，本文档自己的 v1→v2 Low 2 早就说对了，只是这处举例没跟上）。三处逐一改正。
- **Low 2**：重扫的两条「按失败关闭」分支（目录核对不过、包彻底找不到）仍然写着「去 `agent24 os list` 看原因」——跟判据 12 已经确立的规则矛盾（这个分支下 `os list` 根本不会显示新原因）。两处改成直接指向这次应答本身和具体的补救动作（检查目录权限/确认是不是被卸载）。
- （Critical/High：本轮无——机制的三个核心判定：只查交付方式、用 `package_dirs` 分辨 provenance、恰好一条同名报告才触碰，逐条核实为正确，不再有结构性问题。）

## v5 → v6 改动（Codex 第 5 轮：0 Critical、1 High、1 Medium、2 Low，全部采纳）

- **High 1**：v5 把「按目录匹配」和「不再比对记住的名字/版本」两件事合在一起做过了头——目录匹配只能证明「这是当初那个包目录」，不能证明「这个目录里的清单现在还叫请求的这个名字」。清单被改名（交付方式仍合法）会被按目录找到、交付方式检查通过、放行——但下次重启 catalogue 只有新名字，旧名字的启用位永远生效不了，正是本设计要消灭的坏状态。补回**名字**这一条检查（不是版本——名字是 `os.json` 的配置键，版本不是）；新增判据 9b 专门钉住这个绕过路径，判据 9 的正对照措辞也跟着订正（名字不同现在由判据 9b 的名字检查挡住，不是「按目录匹配不到」）。
- **Medium 1**：v5 仍然说「`packages_root` 已经算得比 `AppState::new` 早」——第二次核实，还是错的：真实顺序是 `AppState::new` 在最前，`state_dir`/`packages_root` 都在它之后才算。改成明确说「需要把这两步的计算从现在的位置挪到 `AppState::new` 之前」——这是一次实打实的顺序调整，不是「反正已经早」。
- **Low 1**：判据 13 混用了「整个 `packages_root` 消失」和「单个包子目录消失」两种不同的失败原因——但 `check_packages_root` 会在根目录本身有问题时先一步失败，根本走不到「按目录匹配」那一支；根目录 `read_dir` 失败时 `scan.refused` 塞进去的 `dir` 是根目录本身，不是任何具体包的目录，用按包目录匹配的逻辑去认它会认错。拆成两条：判据 13 只测「单个包子目录消失，根目录本身没事」，判据 15 专测「根目录本身核对不过/读不了」，两条的措辞和归属都分开写清楚。
- **Low 2**：决策记录里一处提前引用了后来改名的字段（写「新增的 `package_names`」，但下面「设计」段实际定的是 `package_dirs`）；两处 v3→v4 的改动标签写错成第 4 轮的（应为 v4→v5）。三处逐一改正。

## v6 → v7 改动（Codex 第 6 轮：0 Critical、0 High、0 Medium、2 Low，全部采纳——终审通过）

- **Low 1**：判据 15 只覆盖了 `check_packages_root` 自己会拒的场景（符号链接/全局可写）——`check_packages_root` 只核对元数据，不实际 `read_dir`，一个属主对但权限位是「只写不可读」的目录能通过这一步核对，随后 `scan()` 自己 `read_dir` 失败，塞进 `scan.refused` 的 `dir` 是根目录本身。按包目录匹配的逻辑看不懂这条记录，会误落进「这个包的子目录不见了」那支，报出一句文不对题的消息。补上：扫描结果先看有没有一条 `dir == packages_root` 本身，有就按根目录失败处理，不往下走到按包匹配；判据 15 拆成 15a（核对本身拒）/15b（核对过了但读不了），15b 同时也是判据 13/14 的负例。
- **Low 2**：两处总结性文字（决策记录收尾句、「不改的东西」列表）还停留在 v5 的措辞——只说「重扫只检查交付方式」，没提 v6 刚补回的名字检查，跟「设计」段的真实算法、判据 9b、上一节的改动记录自相矛盾。两处都改成如实说「先比名字（跟请求的名字本身比，不是跟记住的旧值），再查交付方式，都不比版本」。
- （Critical/High/Medium：本轮无——机制本身逐条核实完毕，只剩文档一致性收尾。）
