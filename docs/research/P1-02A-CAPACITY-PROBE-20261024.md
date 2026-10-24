# P1-02a 合成容量探针（2026-10-24）

## 口径声明
- **200000 行 / 268435456 字节（256 MiB）只是 `agent24-memory` migration `0014_owner_usage_quota.sql` 的静态配置**（`mem_owner_quota` 的 `'*'` 行，migration 注释自称 "Guessed, not measured"），**不是本次磁盘测量，也不是这台机器的测量**。
- `mem_owner_usage.byte_count` 仅统计 payload 字节，不含 scope/causal/索引开销（见 migration L1 注释）。
- 只用临时目录的隔离合成库；未读任何用户私有库；未改 migrations / 配额行为，未启动 purge。

## 方法
测试 [`p1_capacity_probe.rs`](../../rust/crates/agent24-memory/tests/p1_capacity_probe.rs)：临时目录建 `KvStore`，经 `EventLog::append` 写 20000 条合成事件（单 owner，payload `{"text": 256×'x', "i": n}`），`wal_checkpoint(TRUNCATE)` 后测量。

```
export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target; ulimit -n 4096
cd rust && cargo test -p agent24-memory --test p1_capacity_probe -- --nocapture
```
退出码：**0**（1 passed，12.94s）。

## 实测（20000 行合成数据，本机 macOS 临时目录）
| 项 | 值 |
|---|---|
| 行数（`mem_events`） | 20000 |
| payload 字节（`LENGTH(CAST(payload AS BLOB))` 之和，与 `mem_owner_usage` 一致） | 5,528,890 |
| 文件大小（`metadata.len`） | 9,891,840 |
| 磁盘分配（`st_blocks*512`） | 10,551,296 |

衍生：约 276 B payload/行；约 494.6 B 文件/行；文件 ≈ 1.79× payload。

## 解读与限制
- 仅一个合成形态（小 payload，单 owner，含默认索引/FTS 触发器影响），**不代表真实历史或附件**；线性外推到 200000 行约 99 MB 文件仅为粗估，**未测**。
- 按本合成形态，行数配额（200000）先于字节配额（256 MiB）触顶；payload 接近 64 KiB 的事件则相反。该结论需 P1-02b 用真实分布复核。
