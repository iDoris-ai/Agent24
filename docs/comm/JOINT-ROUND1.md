# Agent24 × Hyphae CLI 联调记录 · 第一轮

> 范围：COMM-1a（`agent24-comm` / `HyphaeRunner`）与 Hyphae CLI 的第一轮 CLI 级联调。
> 只做联调和记录，**不改动任何产品代码**。对应设计文档：`docs/design/COMM-HYPHAE.md` §8.2（本轮覆盖该节 1–5 步；6「积压 125 条」、7「zero-run」、8「秘密扫描」留待后续轮次，daemon/REST 路由本轮不涉及）。

## 0. 基线

| 项 | 值 |
|---|---|
| Agent24 分支 / HEAD | `test/comm-joint-round1`，基于 `main` `548ccae` |
| Hyphae source SHA | `a4aa606eb81d5c040d94c51cdf94553e646d8674`（与 `hyphae.lock.json` 的 `source_sha` 一致） |
| 平台 | `darwin-arm64`（本机 `uname -a`: Darwin ... 25.4.0 arm64） |
| Go 版本 | `go1.26.4`（与 lock 的 `go` 字段一致） |
| Hyphae CLI 二进制 sha256 | `f53c29b31d8ca5eb0124ced246bcff6610f048f18bc8dcc2de27f685dad8b221` |
| 与 `rust/crates/agent24-comm/hyphae.lock.json` 的 `darwin-arm64` 条目比对 | **一致**（独立用 `shasum -a 256` 复算过一遍，而不是只信任文件名） |
| `hyphae-relay` 二进制 sha256 | `a012d86e549cbeb564d5a5932c54f9b3511c2434203846537096420c89f36aef`（仅记录，不进 lock——COMM-HYPHAE.md §8.1：relay 运行期不被 comm 使用） |
| A 侧校验路径 | **生产路径**：`HyphaeLock::embedded()` + `expected_for("darwin-arm64")`，**未使用** `test-lock-override` |

结论：二进制 hash 与 lock 一致，A 侧走的是生产校验路径，不是测试覆盖路径——满足 §8.2 步骤 1 的验收条件。

## 1. Harness

- 文件：`rust/crates/agent24-comm/tests/joint_round1.rs`（`#[ignore]`，需要 `HYPHAE_JOINT_BIN` 与 `HYPHAE_JOINT_RELAY` 两个环境变量，否则打印原因后直接返回）。
- 运行方式：
  ```
  HYPHAE_JOINT_BIN=<hyphae 二进制路径> HYPHAE_JOINT_RELAY=<hyphae-relay 二进制路径> \
    cargo test -p agent24-comm --test joint_round1 -- --ignored --nocapture
  ```
- A 侧：`agent24_comm::{HyphaeLock, VerifiedBinary, HyphaeRunner, Invocation, Password}` 公开 API；`VerifiedBinary::install` 走生产 lock。
- B 侧：裸 `std::process::Command` 调同一个二进制，独立临时 `HOME`，口令经 stdin pipe 写入后立即关闭；用 `agent24_comm::parse_envelope`（crate 导出的纯函数）解析 envelope，确保两侧校验的是同一套 envelope 契约。
- 两套 HOME、relay 的 `data-dir`、A 的 verified-binary 安装目录全部在一个 `tempfile::TempDir` 下，退出时自动删除；全程未触碰真实 `~/.hyphae`。
- relay 端口：绑定 `127.0.0.1:0` 取系统分配的空闲端口。
- 口令：均为本轮合成的测试夹具值，非真实口令，原文不进文档——见 `joint_round1.rs` 里的 `PASSWORD_A`/`PASSWORD_B` 常量。
- 本记录中的 npub 保留原文；未涉及 nsec（`identity export` 不在本轮范围内）。

## 2. 实测结果

以下退出码、字段名全部来自 2026-10-01 的真实一次运行（`cargo test ... -- --ignored --nocapture`，退出码 0，1 passed）。口令已脱敏。

### 步骤 1：双方 identity create

| 操作 | 命令（口令脱敏） | 退出码 | 结果 |
|---|---|---|---|
| A | `hyphae identity create --nickname a --password-stdin` | 0 | `{"default":true,"encrypted":true,"nickname":"a","npub":"npub1jh33d...rnk4xnsgqx4e9"}` |
| B | `hyphae identity create --nickname b --password-stdin` | 0 | `{"default":true,"encrypted":true,"nickname":"b","npub":"npub1ue3mq...94h6578sxxku8e"}` |

两侧 `encrypted:true`，符合预期。

### 步骤 2：互加联系人 + relay set/list

| 操作 | 命令 | 退出码 | 结果 |
|---|---|---|---|
| A | `hyphae contact add --nickname b --npub npub1ue3mq...` | 0 | `ok:true` |
| B | `hyphae contact add --nickname a --npub npub1jh33d...` | 0 | `ok:true` |
| A | `hyphae relay set --relay ws://127.0.0.1:<port>` | 0 | `{"relays":["ws://127.0.0.1:<port>"],"source":"config"}` |
| B | `hyphae relay set --relay ws://127.0.0.1:<port>` | 0 | 同上 |
| A | `hyphae relay list` | 0 | `source:"config"` ✓ |
| B | `hyphae relay list` | 0 | `source:"config"` ✓ |

### 步骤 3：A→B（"joint-1"），B 读取

| 操作 | 命令 | 退出码 | 关键字段 |
|---|---|---|---|
| A | `hyphae agent msg --from a --to <npub_b> --content joint-1 --password-stdin` | 0 | `published_to:1`、`history_stored:true`、`event_id:3af4d797e0...bb1ce59` |
| B | `hyphae history inbox --as b --limit 10`（拉取前） | 0 | `data:[]`（空） |
| B | `hyphae agent inbox --as b --password-stdin` | 0 | 返回含 `event_id:3af4d797e0...`、`content:"joint-1"` 的数组 |
| B | `hyphae history inbox --as b --limit 10`（拉取后） | 0 | `id` 与 A 发送的 `event_id` **一致** ✓ |

**发现 F1**：`history inbox` 是拉取式（pull-based），不是推送式。B 必须先执行 `agent inbox --as b --password-stdin`（**必须带 `--password-stdin`**，否则对加密 keystore 直接返回 `auth_error`/退出码 3；手测确认 `--decrypt` 标志不是必需的——不加它消息仍被自动解密并写入 history，`agent inbox` 的输出里 `decrypted` 也始终是 `true`），`history inbox` 才会出现新消息。这与 COMM-HYPHAE.md §5.1「L1/L2 分层由 `published_to` 判定」的叙述不矛盾，但该文档没有明说 B 侧看到消息前必须主动拉取一次——建议在文档里补一句。

### 步骤 4：断线重试

| 操作 | 命令 | 退出码 | 关键字段 |
|---|---|---|---|
| （停 relay，按 pid kill，非 pkill） | — | — | — |
| A | `hyphae agent msg --from a --to <npub_b> --content joint-2 --password-stdin` | 0 | `published_to:0`、`queued_for_retry:true`、`event_id(E2):2cd0383de6...9b5ba71` |
| A | `hyphae storage outbox list` | 0 | 含 `id:E2`，`status:"pending"` |
| （同端口、同 data-dir 重启 relay） | — | — | — |
| A | `hyphae storage outbox retry --id E2` | 0 | `{"attempted":true,"sent":true,"queued":false,"marked_failed":false,...}` |
| B | `hyphae agent inbox --as b --password-stdin` | 0 | 含 `event_id:E2` |
| B | `hyphae history inbox --as b --limit 10` | 0 | 含 `id:E2` ✓ |
| B | 再次 `agent inbox` + `history inbox` | 0 | `E2` **只出现一次**（`e2_count==1`），无重复 ✓ |

全部与 §8.2 步骤 4 的预期一致：`ok:true` + `published_to:0` + `queued_for_retry:true`；retry 后 B 恰好收到一条该 event_id 的消息；重复拉取不产生重复记录。

### 步骤 5：错误路径

| 操作 | 命令（口令脱敏） | 退出码 | error | 是否符合预期 |
|---|---|---|---|---|
| A 用错误口令发送 | `hyphae agent msg --from a --to <npub_b> --content should-not-send --password-stdin` | 3 | `auth_error`（message: `failed to unlock keystore`） | 符合（exit 3 / auth_error） |
| A `contact add` 用非法 npub | `hyphae contact add --nickname badguy --npub not-a-valid-npub` | 4 | `other_error`（message: `invalid npub: invalid hex key: ...`） | 符合（exit 4 / other_error，COMM-HYPHAE.md G6 的记录） |
| A `agent msg --to` 用非法 npub | `hyphae agent msg --from a --to not-a-valid-npub --content bad-to --password-stdin` | 1 | `user_error`（message: `'not-a-valid-npub' is not a known nickname or valid npub`） | 符合 Hyphae 文档的说法：发送路径返回 `user_error`，与 `contact add` 的 `other_error` 不同 |

### 清理

relay 与 harness 内所有子进程均按各自 pid 停止（`Child::kill()` 针对具体 pid，**全程未使用 `pkill -f`**）；两套临时 HOME、relay data-dir、A 的 verified-binary 安装目录都在 `tempfile::TempDir` 的 drop 中被删除；真实 `~/.hyphae`、`~/.agent24` 全程未被触碰。

## 3. 发现列表

| # | 发现 | 影响 | 建议 |
|---|---|---|---|
| F1 | `history inbox` 是拉取式：B 必须先对加密 keystore 执行 `agent inbox --as <nick> --password-stdin`，新消息才会落到 `history inbox`；`--decrypt` 标志本身不是必需的（不加它消息仍会被自动解密、`decrypted` 字段仍为 `true`） | 不是 bug，但 COMM-HYPHAE.md §5.1/§8.2 没有明说这个先后关系，容易被按「push」模型误实现 daemon 轮询逻辑 | 在 COMM-HYPHAE.md 里补一句「`history inbox` 不主动拉取 relay，daemon 的 `--watch-interval` 负责周期性调用 `agent inbox` 才能让 `history inbox` 看到新消息」 |

其余所有步骤（hash 校验、身份创建、联系人、relay 配置、双向发送、断线重试、重复拉取去重、三类错误路径）均与预期**完全一致**，未发现偏差。

## 4. 结论

- COMM-1a 的 `HyphaeRunner`（生产 lock 校验路径）与真实 Hyphae CLI 二进制在本轮全部已跑步骤上互通：envelope 的 `ok`/`error`/`data` 字段、退出码语义（0/1/3/4）、`published_to`/`queued_for_retry`/`event_id` 等字段均与 `agent24-comm` 的 `parse_envelope` 契约吻合，B 侧裸调用同一个 `parse_envelope` 也能正确解析，说明该解析逻辑对双方都成立，不是只对 A 自己构造的调用方式生效。
- 断线重试链路（发送失败入队 → outbox list 可见 → 同 id 重试 → 对端收到且无重复）在真实二进制 + 真实 relay 重启场景下验证通过。
- 三类错误路径（口令错误 → `auth_error`/3；`contact add` 非法 npub → `other_error`/4；`agent msg --to` 非法 npub → `user_error`/1）全部与设计文档的既有说法（含 G6）一致。
- 本轮未覆盖 §8.2 的步骤 6（积压 125 条消息）、7（zero-run 判据）、8（秘密扫描）与 daemon/REST 路由，留给下一轮联调（COMM-3/4a 对应的 daemon 监管与持久化状态机就绪后）。
- 未发现需要改动 `agent24-comm` 生产代码的问题；唯一的文档类发现（F1）建议回填进 `docs/design/COMM-HYPHAE.md`。
