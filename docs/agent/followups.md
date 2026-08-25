# Follow-ups ledger（append-only · 永不删行 · 提交进仓库）

> pilot 的 review triage 把「真问题但不阻塞（B）」和延后项记在这里。
> 主线 task 全部完成后，由 `pilot run` 批量合成一个 cleanup PR 做掉，逐条标 [x] done=PR#n。
> `- [ ]`=OPEN，`- [x]`=DONE。GitHub PR comment 是永久兜底。

- [ ] FU-1 · B · src=PR#140 · 2026-08-23 · rekey 失败后 record_os_partition 撞的是 (org_id,space_id) UNIQUE，ON CONFLICT(owner_key) 接不住 → 冒出裸 Sqlx 错误而非 typed Conflict；运维日志因此说不出「有陈旧 v1 行占着这个 (org,space)」
- [ ] FU-2 · B · src=PR#140 · 2026-08-23 · open_memory_base (server.rs:243) 在记忆库打不开时只 warn! → daemon 可能长期在「无模块记忆也无会话记忆」下运行。提到 error!
- [ ] FU-3 · B · src=PR#140 · 2026-08-23 · sweep 的安全性论证只讲进程内挂载顺序；真正挡跨进程的是 try_acquire_singleton() 与临时 daemon 走 open_memory()，两处注释都没写。且 KvStore::open 是 public，别的进程仍可 attach 同一文件
- [ ] FU-4 · B · src=PR#140 · 2026-08-23 · 首次插入时无人检查 owner_key 是否真的编码了所声明的 (org_id,space_id)。carol 可抢先用 alice 未创建的 key 在自己 org 下 record，alice 从此永久拿不到该分区且无修复路径（可达性为零）。要么写成 record_os_partition 文档里的显式非属性，要么把一致性断言上提到 OsMemoryCatalog::record
- [ ] FU-5 · B · src=PR#140 · 2026-08-23 · Conflict 消息只打呈递的身份、不打已存的 → guard 触发时日志说不清是哪个 org 占着那一行
- [ ] FU-6 · B · src=PR#141 · 2026-08-23 · spec.md:31 usr: 与 os: 前缀的不相交性依赖模块名为 ASCII；T1.2.1 已要求补交叉积测试，确保测试真的覆盖非 ASCII 输入
- [x] FU-7 · B · src=PR#141 · 2026-08-23 · git-guard.sh 的 --allow-trunk 只查 classic branch protection 端点，对 ruleset 保护的分支误判为无保护。修法：classic 报 Branch not protected 时回落查 /rules/branches/{branch}。属 pilot skill，不在本仓库 · done=PR#141
- [ ] FU-8 · **决策待定** · src=reference-notes/macro.md §11.2 · 2026-08-25 · **T1.1.1 的 `Decision{allow,reason}` 是可被忽略的返回值** —— 调用点写 `let _ = decide(..)` 就绕过了，而 architecture.md 自己说「贵的是调用点」。Macro 的 `EntityAccessReceipt<T>`（私有字段 + 唯一会校验的构造函数 + 领域方法要求收据）把「忘了检查」变成编译错误。**建议改成 `authorize() -> Result<AccessGrant, Denied>` + `lend(grant)`。这条要在 T1.1.1 开工前拍板**，之后改就是「另一个量级的工作」。未验证：F8 全部 lend 调用点是否都拿得到 grant
- [ ] FU-9 · B · src=reference-notes/macro.md §11.1 · 2026-08-25 · 「模块永远拿不到 KvStore」今天靠私有字段 + 复审纪律。Macro 用 cargo feature 把层间依赖变成构建保证。修法：agent24-memory 分 root/scoped feature，模块侧只开 scoped，KvStore 在那个构建里不存在
- [ ] FU-10 · B · src=reference-notes/berd.md §10.2-D · 2026-08-25 · **契约漂移 CI**：`os list` 输出形状变了没有任何东西会红；SPEC-ME-FOLLOWUPS F4a 抓的「lint:openapi 抓不到少写端点」是同一个病的另一个病灶。修法学 berd 的 `generate-berdctl-contract --check`（从命令模块生成契约、与签入的对比）。berd 是 Apache-2.0，脚本可直接借用 + 保留版权头
- [ ] FU-11 · **需先核实** · src=reference-notes/berd.md §10.2-F · 2026-08-25 · berd 把「CLI 校验只是便利、任何同用户进程都能绕过 CLI 直接打 broker」写成契约，信任边界放在最内层 registry。**待查：agent24d 的 HTTP 面是否独立校验了 `agent24 os` 传来的输入？** 若否是真实的洞。核实前不得当成已知缺陷陈述
- [ ] FU-12 · B · src=reference-notes/macro.md §11.3 · 2026-08-25 · frecency（frequency × recency）作为一等排序键 —— 语义检索抓不到「我最近反复碰过的东西」这一维。F1.3 之后对记忆召回质量是便宜的大改进
- [ ] FU-13 · B · src=reference-notes/berd.md §10.3-I · 2026-08-25 · oMLX / 模型权重的版本管理今天靠约定。学 berd 的 goose-backend.lock.json：lockfile 钉死 + 缓存与 lockfile 不一致时**直接 fail，不静默回退**
- [ ] FU-14 · **决策待定** · src=reference-notes/berd.md §9 · 2026-08-25 · **ME-3（进程外 Provider）开工前需一条 ADR 裁决「自定义协议 vs ACP」**。Berd 与 Macro 两个互不相干的团队都用 ACP 做「壳 ↔ agent 运行时」边界。理由不是从众，是互操作。未验证：ACP 的能力覆盖度、与审批门/ScopedMemory 的契合度
- [ ] FU-15 · B · src=reference-notes/berd.md §10.1-A · 2026-08-25 · **建 `docs/laws/`**：把 architecture.md 的「不可动摇的边界」+ SPEC-ME-FOLLOWUPS F1 的「不得声称的话」抽成编号的 MUST/MUST NOT 条款，并在 PR 模板加「本 PR 影响哪几条法律」。F1/F8 二十余轮复审抓到的几乎全是「措辞比机制强」= 法律与实现不一致，今天靠人肉记忆发现
