# F5 — 7×24 稳定性泡测 Runbook

> **F5 是物理任务**：Mac mini 连续 **7 天**、定时工作流照跑、**无人工干预**。
> 代码侧全部就绪（F1a 开机自启 · F2 崩溃自愈 · F3 微信 · F4 Nostr），此文把「按下起跑」变成一条可照做的清单。这份 runbook + `scripts/soak-monitor.sh` 是能替你做的最大块；7 天的时钟只能在你的机器上走。

## 判定标准（跑完对照）

一次通过的泡测应满足：

1. **可用性**：`soak-monitor` 记录的 health 命中率 **100%**（偶发 daemon 重启允许——那正是 F2 要证明的——只要 health 每次都恢复）。
2. **调度器全程存活**（不是"`/schedules` 里还有行"）：任何采样点都 **没有 `overdue`**（`next_run_at` 落在过去仍未触发）、**没有 `auto_disabled`**（连续失败 5 次后 daemon 把 schedule 置 `enabled=false, next_run_at=null`，行还在但调度器已死）、**没有 `fetch_errors`**（`/schedules` 返回 401/500 错误信封被误当成计数）。这三条任一出现即判 **NEEDS REVIEW**——一个"死掉但行还在"的调度器**不算通过**。
3. **无人工干预**：7 天里你没有手动重启 daemon / 重连渠道 / 清状态。
4. **无内存泄漏**：`rss_mb` 曲线平稳，不单调爬升。
5. **渠道存活**：微信 / Nostr 入站在第 1 天和第 7 天都能触发 run + 审批回。
6. **Nostr 入站通路全程活着**（FU-32）：由 `soak-monitor.sh` 采样健康快照自动判定，判据见下面「起跑」一节的完整列表。要点：`degraded_transitions` **在本次 run 内不增长**（不是「必须为 0」——它是跨重启累计的终身计数，历史值不该让以后每次泡测都失败），快照不能陈旧、不能读不出来、`generation` 不能变。**不配置 Nostr 的 run 必须显式加 `--no-nostr`**——「没有证据」不会被当成「没配置」。

   > 判据用的是**累计计数 + 新鲜度**，而不是「我去看的时候 state 是 ok」：健康文件是原地覆盖写的，周二坏掉、周三自己好了的话文件里**不留任何痕迹**；而桥要是第一天就死了，文件会**冻结在 `ok` 上**——陈旧的证据和健康的证据长得一模一样，这正是 FU-32 本身那个病。
   >
   > 别为了「让判据变绿」去删健康文件：那会连跨重启的静默账、`self_npub` 和 `generation` 一起丢掉，而 `generation` 变化本身就是失败条件。

   > 第 5 条单独**挡不住**这一类失效,这正是 FU-32 的教训:桥读的是 agent-speaker daemon 填的**本地库**,daemon 那侧 relay 断了的话 `history inbox` 照样 exit 0 返回 `[]` —— 不抛错、不超时、进程健康、日程照跑,**只是谁的消息都收不到**。只在第 1 天和第 7 天各发一条,中间六天全哑也照样 PASS。桥现在每 5 分钟给自己发一条 canary,只有 daemon 真把它从 relay 拉回来才算数;15 分钟没有确认就写 `degraded` 并在 stderr 报警。**睡眠→唤醒之后必须专门抽查一次**——那是这条最可能失效的时刻。

`soak-monitor.sh` 退出时按 **1、2、6** 自动给 PASS / NEEDS REVIEW；3–5 靠你日常抽查 + 日志。

判据 6 由脚本采样健康快照来判，这些情况判失败：任何一次采样抓到 `state=degraded`；`degraded_transitions` **在本次 run 内增长**；快照读不出来 / 格式非法 / 属于另一个身份；快照**超过 15 分钟没更新**（进程死了但文件还在，冻结在 `ok` 上）；`confirmed` 或 `degraded_transitions` **回退**，或 `generation` 变化（账本被重置——重置会把跨重启的静默一起抹掉）；`confirmed` 全程没涨；整轮**从没读到过快照**，或快照**迟到超过 15 分钟才出现**（在那之前那一段没有任何入站证据）。

确实不跑 Nostr 的 run 要显式加 `--no-nostr`——「没有证据」不会被当成「没配置」。

脚本还有两个「这不是 F5 结论」的出口，都**以非 0 退出**：跑得比 15 分钟还短的 run 报 `SMOKE PASS`（那种长度根本评估不了判据 6）；没跑满 `--duration` 就被 Ctrl-C / `kill` 中断的 run 报 `INCOMPLETE`（7 天的 run 在第 20 分钟被掐掉，采样到的一切当然都是健康的——那正是陷阱）。

快照必须自报身份（`context.identity`），监控只认与 `--nostr-identity`（默认 `agent24`）一致的那一份：否则把 `--nostr-health` 指到另一个桥的文件上，就成了拿别人的健康替这一个背书。

> 监控这道门本身有回归测试：`scripts/test-soak-monitor.sh`（起一个假 daemon，覆盖 18 种情形，PASS 与各种失败两个方向都测）。改动 `soak-monitor.sh` 后跑一下。

## 一次性准备

```bash
# 1) release 构建（泡测用 release，别用 debug）
cd rust && cargo build --release -p agent24d -p agent24-cli
sudo cp target/release/agent24 target/release/agent24d /usr/local/bin/   # 两个都要:`agent24 service install` 会找同目录的 agent24d(否则 ENOENT)

# 2) 开机自启 + 自愈（F1a/F2）
agent24 service install          # 装 LaunchAgent，登录即起、崩溃自拉
agent24 service status           # 确认 running

# 3) 造几条“日常”定时任务（泡测的负载——照你真实用途，至少覆盖各时段）
#    经 CLI 或桌面端 Schedules 页建；例如每小时一条轻量 run、每天早/晚各一条。
agent24 schedules list           # 确认 next_run_at 合理

# 4) 渠道授权（各一次）
#    微信：起 wechat-bridge，首跑打印二维码，用微信扫码绑 bot（token 存本地，之后免扫）
pnpm --filter @agent24/wechat-bridge start   # 扫码
#    Nostr：建 identity（见 F4-nostr-channel.md），配 npub 白名单
```

### ⚠️ 已知坑（TASKS.md 记录）

- **先确认 agent-speaker 的二进制名（FU-34）**。上游已把它改名为 `hyphae`（`cmd/hyphae/`、module `github.com/iDoris-ai/hyphae`），而本仓默认还找 `agent-speaker`。起跑前跑一遍：

  ```bash
  which agent-speaker || which hyphae      # 装的是哪个名字
  # 若是 hyphae，桥要显式指过去（否则第一分钟就 degraded）
  export A24_SPEAKER_BIN=hyphae
  # 两条只读冒烟:都要返回 {"ok":true,...} 信封
  $A24_SPEAKER_BIN identity list --json
  $A24_SPEAKER_BIN history inbox --as agent24 --limit 5 --json
  ```

  这两条只覆盖桥的**读**路径。`agent msg` 与 `profile publish` 的改名后契约**仍未验收**（F4 联调是对着改名前的 7cef326 验的）。桥起来后分别这样确认：

  - `agent msg` → `cat ~/.agent24/nostr-bridge-health-<identity>.json`，`last_error` 为 null 且 `canaries.sent` 在涨（canary 就是走这条命令发的）。
  - `profile publish` → 看桥的启动日志里有没有 `[nostr] ✅ 已注册能力,发布到 N 个 relay`；失败会打 `[nostr] 注册失败`。**它不会写进健康快照的 `last_error`**（那个字段只来自活性探针），所以别用它证明注册成功。

- **泡测的 daemon 要关掉桌面通知和自动回复**：`hyphae daemon --notify=false --auto-reply=false`。桥每 5 分钟发一条 canary，daemon 会把它当成普通入站消息处理 —— `--notify` **默认是开的**，7 天会弹约 2000 次通知并播 2000 次提示音；`--auto-reply` 开着还会为每条 canary 多产生一个 relay 事件。桥侧的过滤发生在这之后，挡不住这一层（FU-33 已记：上游应给探针留一个 tag 并跳过通知/自动回复）。

- **桥和 daemon 必须watch 同一个 relay**。`hyphae daemon --relay X` 而桥 `A24_NOSTR_RELAY=Y` 的话，canary 发出去没人收 → 一直 `degraded`，而且症状和"通路真的死了"完全一样。

- **launchd 不继承登录 shell 的环境变量**。凡是 daemon 需要的 env（`OMLX_URL`、`OMLX_API_KEY`、API keys、`A24_*`），必须写进 LaunchAgent plist 的 `EnvironmentVariables`，不能只 `export` 在 `~/.zshrc` 里——否则自启的 daemon 连不上模型。装完 `service install` 后核对 plist。

## 起跑

```bash
# 后台跑监控（默认 7 天、每 5 分钟采样一次）；用 nohup 让它脱离终端
nohup scripts/soak-monitor.sh --log ~/agent24-soak.jsonl > ~/soak-monitor.out 2>&1 &

# 先来个 1 小时冒烟确认监控本身没问题，再放 7 天：
scripts/soak-monitor.sh --interval 60 --duration 3600
```

监控只读（`GET /health` + `GET /schedules`），不碰 daemon 状态。它把每次采样写成一行 JSONL：

```json
{"at":"2026-08-21T09:00:00Z","health":true,"code":"200","pid":4123,"restarts":0,"rss_mb":38.2,"cpu_pct":0.1,"schedules":3,"overdue":0}
```

## 期间抽查（不算“干预”，只是看）

```bash
# 存活率 / 重启数快照
jq -s '{samples:length, ok:map(select(.health))|length, restarts:(max_by(.restarts).restarts)}' ~/agent24-soak.jsonl
# 内存是否在爬
jq -r '[.at, (.rss_mb|tostring)] | @tsv' ~/agent24-soak.jsonl | tail -50
# 有没有出现过 overdue schedule
jq 'select(.overdue > 0)' ~/agent24-soak.jsonl
# 第 1 天 / 第 7 天各发一条真实微信、Nostr 消息，确认能驱动 run + 审批回

# Nostr 入站通路现在是活的吗（FU-32）——每次开机/唤醒后都看一眼
cat ~/.agent24/nostr-bridge-health-agent24.json   # 文件名带身份后缀
# state 应为 "ok"；confirmed 应随时间增长；generation 应保持不变
#（计数与 generation 都跨重启累计/继承，launchd 重启不会归零 —— 所以判据看的是
#  degraded_transitions 在本次泡测期间有没有涨，而不是它是不是 0）
# lost 偶尔 >0 是已知的上游竞态（FU-33）；只要 confirmed 在涨、transitions 是 0 就不算问题
# "degraded" = 对端消息现在收不进来，按报警里那三条顺序查（daemon 在跑吗 / relay 一致吗 / 网络回来了吗）

# 整个泡测期间出现过 degraded 吗 —— 看采样序列，不要只看当前文件
jq 'select(.nostr_degraded != null and .nostr_degraded > 0)' ~/agent24-soak.jsonl
jq -r '[.at, (.nostr_state|tostring), (.nostr_confirmed|tostring)] | @tsv' ~/agent24-soak.jsonl | tail -20
```

## 收尾

`Ctrl-C`（或 7 天到点自停）→ 监控打印 PASS / NEEDS REVIEW 摘要。把摘要 + `~/agent24-soak.jsonl` 归档，据此在 `docs/specs/TASKS.md` 把 **F5** 标 done、**P1 收尾**。

> 远程支持：把 `~/soak-monitor.out` 的摘要行 / 异常行贴回来，我能帮你判读趋势、定位重启或泄漏根因。
