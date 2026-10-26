# P1-02c-0 容量测量（设计 D9）

> 2026-10-26。基线 `origin/ab/memory-p1` = `6bbd8d95`。设计：`docs/agent/P1-02C-QUOTA-FAILURE-DESIGN.md` D9。

## 口径声明
- **只在合成库上测量**（临时目录里新建的 SQLite，随测试结束删除）。未打开 `~/.agent24` 下任何文件，未读任何用户私有库。
- **不代表用户真实分布**。合成文本、固定的会话长度、固定的「记住」比例，都是本次人为设定。
- **默认限额不因本次测量改变**（`200000` 行 / `268435456` 字节，仍是 migration `0014` 的静态配置）。
- 「达到限额所需轮数」是**线性外推**，不是实测；测量只覆盖 3000 轮。
- `byte_count` 取 `mem_owner_usage.byte_count`，即 `LENGTH(CAST(payload AS BLOB))` 之和，不含索引、scope、页开销。

## 方法
测试 `rust/crates/agent24-agent/tests/p1_02c0_capacity_measure.rs`，两个 `#[ignore]` 用例，不进默认 CI。

走真实 personal 写入路径：`RunManager::chat_remember_turn` -> `SessionMemory::remember` -> `SessionLog::append_turn`（user + assistant 两行，一个事务）-> 超过 `max_recent=40` 时真实压缩（`append_summary`）-> 用户说「记住 …」时 `retain::persist` -> `WriteGate::propose`（写 assertion 和 `mem.write_decision` 审计事件）。放在 `agent24-agent` 而不是 `agent24-memory`，因为 `SessionMemory::remember` 和 `retain::persist` 是 `pub(crate)`，只能经 `chat_remember_turn` 这个公开入口到达。

合成对话形态（字符数均匀分布；文本约 35% 汉字、65% 小写字母和空格，所以 UTF-8 约 1.7 字节/字符）：

| 档 | 用户消息 | 助手消息 |
|---|---|---|
| 短对话 | 20–80 字符 | 100–400 字符 |
| 长对话 | 200–1500 字符 | 1500–6000 字符 |

共同设定：每档 3000 轮；每 100 轮换一个 session；每 20 轮（5%）的用户消息以「记住 」开头触发显式记住；压缩用固定 800 字符的合成摘要（真实摘要大小未知）；默认压缩策略（40 条触发、保留 20 条）；单 owner。`wal_checkpoint(TRUNCATE)` 只在末尾执行一次；WAL 峰值取每轮之后对 `-wal` 文件长度的采样最大值（SQLite 默认 autocheckpoint 开启，未人为关闭）。

```
export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target; ulimit -n 4096
cd rust && cargo test -p agent24-agent --test p1_02c0_capacity_measure -- --ignored --nocapture --test-threads=1
```
退出码 **0**（2 passed）。用例逐轮断言写入回执（显式记住轮必须是 `Saved`，其余轮必须是 `NotApplicable`），结束时断言 `message` / `session.summary` / `mem.write_decision` 三种 kind 的行数等于按合成形态算出的预期值（6000 / 240 / 150），因此部分写入失败会使用例 panic 而不是低估容量。加这些断言后重跑，数字与上表逐项相同。

机器：Apple M1 Max，macOS 26.4.1（`sysctl -n machdep.cpu.brand_string` / `sw_vers -productVersion`），本机 APFS 临时目录。

## 原始数字

每 1000 轮新增（三个 1000 轮块，几乎相同）：

| 档 | 块 | 新增 `mem_events` 行 | 新增 `byte_count`（字节） | 主库 `page_count×page_size` 块末（字节） |
|---|---|---|---|---|
| 短 | 1 | 2130 | 774,298 | 1,875,968 |
| 短 | 2 | 2130 | 780,155 | 3,547,136 |
| 短 | 3 | 2130 | 781,477 | 5,156,864 |
| 长 | 1 | 2130 | 8,235,862 | 11,149,312 |
| 长 | 2 | 2130 | 8,147,714 | 21,897,216 |
| 长 | 3 | 2130 | 8,175,240 | 32,768,000 |

3000 轮合计：

| 项 | 短 | 长 |
|---|---|---|
| `mem_events` 行 | 6390 | 6390 |
| `mem_owner_usage.byte_count` | 2,335,930 | 24,558,816 |
| 主库 `page_count×page_size` | 5,156,864 | 32,768,000 |
| WAL 峰值 | 4,185,952 | 4,194,192 |
| WAL，checkpoint 前（末尾） | 4,185,952 | 4,194,192 |
| WAL，`wal_checkpoint(TRUNCATE)` 后 | 0 | 0 |
| payload 平均 | 365.6 B | 3843.3 B |
| payload P95 | 745 B | 9492 B |
| payload 最大 | 1441 B | 10,379 B |

行的构成（3000 轮）：`message` 6000 行（每轮 2 行）；`session.summary` 240 行；`mem.write_decision` 150 行（150 次显式记住）。字节：短档 message 1.93 MB、summary 0.35 MB、decision 0.06 MB；长档 message 23.96 MB、summary 0.35 MB、decision 0.25 MB。每轮约 2.13 行，其中 0.08 行来自压缩，0.05 行来自显式记住的审计。

## 外推到默认限额（线性外推，非实测；取块 3 的每千轮数字）

| 档 | 到 200000 行 | 到 268435456 字节 | 先到哪个 |
|---|---|---|---|
| 短 | ≈ 93,900 轮 | ≈ 343,500 轮 | 行数 |
| 长 | ≈ 93,900 轮 | ≈ 32,800 轮 | 字节 |

对应的主库文件大小（按块 2 到块 3 的每千轮增量外推）：短档到行数限额时约 155 MB；长档到字节限额时约 355 MB，约为 `byte_count` 的 1.3 倍。页开销比随 payload 大小变化：短档主库增量是 `byte_count` 的 2.1–2.2 倍，长档约 1.3 倍。assertion 表不计入配额但计入主库文件大小，已含在上面的页数里。

## 对 D5 / D6 的观察（只给观察，不改设计）
1. **D5 的 `2×WAL` 项在本测量里不起作用。** WAL 峰值约 4 MiB（接近 SQLite 默认 1000 页 autocheckpoint 的上限），`2×WAL` 约 8 MiB，远小于 512 MiB 下限，所以门槛实际由 512 MiB 决定。该项只在 WAL 因长读事务或 checkpoint 受阻而异常增长时才有意义；本测量没有造这种场景，未验证。
2. **512 MiB 与默认限额同量级，而不是远小于它。** 长档在字节限额处主库约 355 MB，已接近 512 MiB。「剩余空间不低于 512 MiB」这个地板本身没问题，但它同时要容纳本库继续增长、其他进程的写入和 `-wal`；这一点需要 P1-03 展示时让用户看懂，不是否定数字。
3. **256 MiB 的 payload 配额对应的磁盘不是 256 MiB。** 页开销比在 1.3–2.2 倍之间，所以 D5 / D6 里「用量」和「磁盘」是两个不能互相换算的量；D6 把两组告警分开设计是合理的。
4. **D6 的 80% / 95% 对行数档在这个合成速率下留有足够余量**：短档 80%（16 万行）约 75,000 轮，95% 约 89,000 轮，两档之间约 14,000 轮。对长对话的字节档，80% 约 26,300 轮，95% 约 31,200 轮，之间约 4,900 轮；这是极端形态（单轮平均 8 KB），仍然有可操作的缓冲，没有发现 80 / 95 明显不合理。
5. **回落阈值 75% / 90% 的滞回宽度（5 个百分点）**在长档约相当于 1,600 轮；撤回不释放空间，所以滞回主要在 purge / 归档之后才会真正触发，这个设计与测量不冲突。
6. **未覆盖**：接近 64 KiB 的大 payload、附件（`mem_trace_refs` / `mem_artifacts`，配额 SQL 不统计）、多 owner、并发写入、真实摘要长度、`mem_events` 之外的表增长的单独拆分。这些会改变上面的数字，不应据本次结果调整默认限额。
