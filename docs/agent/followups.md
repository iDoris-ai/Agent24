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
- [ ] FU-7 · B · src=PR#141 · 2026-08-23 · git-guard.sh 的 --allow-trunk 只查 classic branch protection 端点，对 ruleset 保护的分支误判为无保护。修法：classic 报 Branch not protected 时回落查 /rules/branches/{branch}。属 pilot skill，不在本仓库
