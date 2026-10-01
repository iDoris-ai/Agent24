# F5 — 7×24 稳定性泡测 Runbook

> **F5 是物理任务**：Mac mini 连续 **7 天**、定时工作流照跑、**无人工干预**。
> 代码侧全部就绪（F1a 开机自启 · F2 崩溃自愈 · F3 微信 · F4 Nostr），此文把「按下起跑」变成一条可照做的清单。这份 runbook + `scripts/soak-monitor.sh` 是能替你做的最大块；7 天的时钟只能在你的机器上走。

## 判定标准（跑完对照）

一次通过的泡测应满足：

1. **可用性**：`soak-monitor` 记录的 health 命中率 **100%**（偶发 daemon 重启允许——那正是 F2 要证明的——只要 health 每次都恢复）。
2. **调度器全程存活**（不是"`/schedules` 里还有行"）：任何采样点都 **没有 `overdue`**（`next_run_at` 落在过去仍未触发）、**没有 `auto_disabled`**（连续失败 5 次后 daemon 把 schedule 置 `enabled=false, next_run_at=null`，行还在但调度器已死）、**没有 `fetch_errors`**（`/schedules` 返回 401/500 错误信封被误当成计数）。这三条任一出现即判 **NEEDS REVIEW**——一个"死掉但行还在"的调度器**不算通过**。
3. **无人工干预**：7 天里你没有手动重启 daemon / 重连渠道 / 清状态。
4. **无内存泄漏**：`rss_mb` 曲线平稳，不单调爬升。
5. **渠道存活**：微信 / Nostr 入站在第 1 天和第 7 天都能触发 run + 审批回。**自 COMM-5a 起**
   Nostr 入站默认冻结(不再触发 run),要验这一条须显式设 `A24_NOSTR_F4B_INBOUND=1`。
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
#    ⚠️ **`agent24 schedules` 这个子命令不存在**（2026-09-09 实测；CLI 只有
#    chat/models/service/daemon/tui/os/mcp）。本文此前两处这么写，是错的。
#    走 API 建（或桌面端 Schedules 页）：
TOKEN=$(python3 -c "import json;print(json.load(open('$HOME/.agent24/daemon.json'))['token'])")
PORT=$(python3 -c "import json;print(json.load(open('$HOME/.agent24/daemon.json'))['port'])")
curl -s -X POST "http://127.0.0.1:$PORT/api/v1/schedules" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"soak-5min","enabled":true,
       "spec":{"type":"every","secs":300},
       "action":{"type":"agent_run",
                 "prompt":"soak heartbeat: reply with the single word OK",
                 "model_override":"Qwen3-0.6B-4bit"}}'
curl -s "http://127.0.0.1:$PORT/api/v1/schedules" -H "Authorization: Bearer $TOKEN"   # 确认 next_run_at

#    ⚠️ **必须至少建一条**：soak-monitor 要求 `schedule_min > 0` 才可能 PASS
#    ——「没人建过 schedule 的泡测什么也证明不了」（脚本 :294 自己写着）。
#    ⚠️ **钉死 model_override**：不钉的话 7 天里可能挑到 27B/35B，白烧内存和发热。

# 4) 渠道授权（各一次）
#    微信：起 wechat-bridge，首跑打印二维码，用微信扫码绑 bot（token 存本地，之后免扫）
pnpm --filter @agent24/wechat-bridge start   # 扫码
#    Nostr：建 identity（见 F4-nostr-channel.md），配 npub 白名单
```

### ⚠️ 已知坑（TASKS.md 记录）

- **先确认 agent-speaker 的二进制名（FU-34）**。上游已把它改名为 `hyphae`（`cmd/hyphae/`、module `github.com/iDoris-ai/hyphae`），而本仓默认还找 `agent-speaker`。本机的编译产物在 `~/Dev/auraai/agent-speaker/bin/hyphae`，**不在 PATH 上**，所以桥要显式指过去。

  好消息：**`--json` 契约没有随改名漂移**（2026-09-02 实测四条：信封形状、`identity list` 的裸数组、`history inbox` 的 `--as/--limit`、`agent msg` 与 `profile publish` 的旗标全在）。起跑前只需确认安装位置与 identity：

  ```bash
  which agent-speaker || which hyphae      # 都没有的话就用仓库里的编译产物
  export A24_SPEAKER_BIN=~/Dev/auraai/agent-speaker/bin/hyphae   # 否则第一分钟就 degraded
  # 两条只读冒烟:都要返回 {"ok":true,...} 信封
  $A24_SPEAKER_BIN identity list --json
  $A24_SPEAKER_BIN history inbox --as agent24 --limit 5 --json
  ```

  第一条现在应该返回**非空**的身份列表——`{"ok":true,"data":[]}` 说明还没建 identity，那样 `history inbox --as agent24` 会返回 `identity 'agent24' not found`，桥起来就是 degraded。

  这两条只覆盖桥的**读**路径。`agent msg` 与 `profile publish` 的改名后契约**仍未验收**（F4 联调是对着改名前的 7cef326 验的）。桥起来后分别这样确认：

  - `agent msg` → `cat ~/.agent24/nostr-bridge-health-<identity>.json`，`last_error` 为 null 且 `canaries.sent` 在涨（canary 就是走这条命令发的）。
  - `profile publish` → 看桥的启动日志里有没有 `[nostr] ✅ 已注册能力,发布到 N 个 relay`；失败会打 `[nostr] 注册失败`。**它不会写进健康快照的 `last_error`**（那个字段只来自活性探针），所以别用它证明注册成功。

- **泡测的 daemon 要关掉桌面通知**：`hyphae daemon --identity agent24 --notify=false`。桥每 5 分钟发一条 canary，daemon 会把它当成普通入站消息处理 —— `--notify` **默认是开的**，7 天会弹约 2000 次通知并播 2000 次提示音（按 5 分钟一发算；若把 `A24_NOSTR_CANARY_MS` 调小，次数按比例上升）；`--auto-reply` 开着还会为每条 canary 多产生一个 relay 事件。桥侧的过滤发生在这之后，挡不住这一层（FU-33 已记：上游应给探针留一个 tag 并跳过通知/自动回复）。

  > **更正（2026-09-09 实测）**：`--auto-reply` 的默认值**已经是 `false`**（`hyphae daemon --help` 逐字确认），不需要显式关。本文此前写「`--notify=false --auto-reply=false`」并说两个都默认开着 —— 前半对，后半不对。

- **桥和 daemon 必须watch 同一个 relay**。`hyphae daemon --relay X` 而桥 `A24_NOSTR_RELAY=Y` 的话，canary 发出去没人收 → 一直 `degraded`，而且症状和"通路真的死了"完全一样。

- **launchd 不继承登录 shell 的环境变量**。凡是 daemon 需要的 env（`OMLX_URL`、`OMLX_API_KEY`、API keys、`A24_*`），必须写进 LaunchAgent plist 的 `EnvironmentVariables`，不能只 `export` 在 `~/.zshrc` 里——否则自启的 daemon 连不上模型。装完 `service install` 后核对 plist。

### 🔴 起跑前必须解决：加密 keystore 会让 headless 完全走不通（R3，2026-09-09 实测撞上）

**症状**：任何 identity 操作都提示输入密码，非交互下直接失败：

```
Keystore password:
{"ok":false,"error":"other_error","message":"failed to read password: operation not supported by device"}
```

**原因**：`~/.hyphae/keystore.json` 一旦 `"encrypted": true`，`hyphae` 在**每一个** identity 子命令前都要解锁——**包括 `identity create` 本身**（`internal/identity/commands.go:57` 的 `if ks.Encrypted { PromptPassword(...) }`）。而 keystore 路径**无法覆盖**：`GetKeyStorePath()` 只用 `os.UserHomeDir()/.hyphae`，没有任何环境变量或旗标（`internal/identity/keystore.go:18-28`）。

**检查**：

```bash
python3 -c "import json;d=json.load(open('$HOME/.hyphae/keystore.json'));print('encrypted =',d.get('encrypted'),'| identities =',list((d.get('identities') or {}).keys()))"
```

**处理**：

- `encrypted = False` → 没事，直接建 identity。
- `encrypted = True` **且 identities 为空** → 那个加密标记是某次失败尝试留下的，**移开重建**（零个 identity = 零把密钥，没有东西会丢；仍然先备份）：

  ```bash
  mv ~/.hyphae/keystore.json ~/.hyphae/keystore.json.bak.$(date +%Y%m%d-%H%M%S)
  hyphae identity create --nickname agent24 --default --json    # 不带 --password ⇒ 不加密、不提示
  ```

  代码依据：只有 `ks.Encrypted` 为真才提示；全新 keystore 不带 `--password` 就落成不加密（`commands.go:57` 与 `:70` 两个分支）。
- `encrypted = True` **且里面有 identity** → **停下问用户**。移开会丢真密钥。这种情况 headless 泡测走不通，要么用户提供一个无密码的独立 identity，要么等上游给非交互解锁（**R3**）。

**验完这两条再往下**（都要 `{"ok":true,...}`，且第一条的 `data` **非空**）：

```bash
$A24_SPEAKER_BIN identity list --json
$A24_SPEAKER_BIN history inbox --as agent24 --limit 5 --json
```

### 🟢 relay：公共 relay 会限流，判据 6 会被第三方绑架（2026-09-09 实测）

三条实测，都带正对照：

| relay | 结果 |
|---|---|
| `wss://relay.aastar.io`（默认，iDoris 自己的） | **下线**。DNS 正常、TCP 443 通、WS 升级返回 **HTTP 530**（Cloudflare 源站不可达） |
| `wss://relay.damus.io` | 第一次发成功，**接着连发三次全失败**（`503` + `publish: context deadline exceeded`）——限流 |
| `ws://localhost:7447`（`bin/minirelay`） | ⚠️ **条件不明，与下文的 2% 不同源，不作为推荐依据** —— 见下 |

canary 每 5 分钟一发、7 天两千次，打公共 relay 必然吃限流，于是 **`degraded` 会是 relay 的错而不是我们的错**，判据 6 被第三方可用性绑架。

> **上表 minirelay 那行要单独说清楚，因为它和下文的 2% 在统计上不可能同源。** 那次读到的是 `sent=4 confirmed=3 lost=0`；若确认率真是 2%，4 次里 ≥3 次确认的概率是 **3.2×10⁻⁵**。所以那行背后有一个变量没被记下来。
>
> **取证做了，但没找到那个变量，如实写**：
>
> - **观测**：`messages.db` 里 11 条 `is_incoming=1` 的 canary，**8 条挤在 19:23–19:27 这 4 分钟**（间隔约 20 秒，正是探针 overdue 加速后的节奏，即那段时间**几乎每一条都确认了**），之后 40 分钟空白，再零星 2 条。不是随机分布，是**有条件的**。
> - **`received_at − created_at`（lag）在窗口内是 0–25 秒**，与「daemon 每 30 秒醒一次、订阅 3 秒」的节律相符。
> - **第 9 条（19:19:22）不属于这一簇，别把它并进去**：它的 lag 是 **115 秒**，是窗口内的 4–20 倍，且发生在切本地 relay 之前的 damus 时期。合并成「19:19–19:27 的 9 条」读起来更整齐，但会**盖掉「它不同源」这个信息**——而那正是本节要说的事。
> - **[正对照 · 时间轴]** `is_incoming=0` 的 canary 有 **152 条，跨度 19:22:26 → 20:50:51**。这一格证明库里有一条长得多的时间轴，所以「8 条挤在 4 分钟」是**真的聚集**，不是「库只覆盖了这 4 分钟」造成的假象。**没有这一格，聚集与采样窗口分不开**——这与给命中数配正对照是同一个动作，只是换到时间维度上。（PR-Daemon 在 #154 补的。）
> - **一个被自己的数据证伪的假设**：我曾猜「经 daemon outbox 重发的 canary 会赢竞态（因为没有后续的 CLI 覆盖）」。对 event id 之后 —— daemon 日志里 12 条 `✅ Sent` 与**确认行、丢失行都零匹配**。**假设不成立，记下来免得下一个人再走一遍。**
> - **仍未验证的假设**（PR-Daemon 提出，我没能证实也没能证伪）：真正的决定变量可能是**「daemon 那一刻有没有订阅同一条 relay」**，而不是「relay 在不在本地」。若真如此，选项表的三行全是**代理变量**，而那个真变量恰好是操作者能控制的 —— 它可能就是一个绕法（让 canary 走一条 daemon 不监听的 relay）。**在验证之前不要照这个思路配置泡测。**
>
> **所以 minirelay 那一行不作为任何推荐的依据。** 下面 2% 那组读数是在同一套配置上连续跑 40 分钟得到的，条件明确，以它为准。

### ⛔ 但**别用本地 minirelay** —— 它会把一个上游竞态从偶发变成必然（2026-09-09 实测推翻了本节的上一版结论）

本节上一版写「结论：F5 用本地 minirelay」。**那是错的，收回。**

实测：切到本地 relay 后，canary **发得出去、daemon 收得到**（`hyphae-daemon.log` 里每条都有 `📨 New message ... a24-liveness-canary`），但桥**一条都确认不了**，`sent=73 confirmed=0 lost=61`，`state` 永远 `degraded`。

**原因是 FU-33 记的那个上游竞态，本地 relay 让它几乎每次都触发**（**不是每次** —— 见下面的读数，别把它写成"永远"）：

- `internal/messaging/agent.go:220` **先** `relay.Publish`，`:241` **才** `StoreOutgoingMessage`；
- 而 store 用的是 `INSERT OR REPLACE`（`internal/storage/message.go:34`）；
- 本地 relay 往返只要几毫秒，于是 **daemon 先把事件写成 `is_incoming=1`**，发送方的 CLI 随后 `INSERT OR REPLACE` **把它覆盖回 `is_incoming=0`**；
- daemon 的 `seen` 集合让它**永不重处理**那个 event id；
- 那行于是对 `history inbox` **永远不可见** → canary 永远确认不了。

**读数，以及一处要更正的过度断言。** 本节上一版写「`confirmed` 永远是 0」——**被我自己的数据推翻了**，如实改：

```
桥的健康快照（跑了约 40 分钟后）：sent=105  confirmed=2  lost=94  degraded_transitions=2
messages.db 含 canary 的行：      is_incoming=0 → 130 行 ｜ is_incoming=1 → 11 行
```

**赢面约 2%**，不是零。竞态是竞态，不是必然——本地 relay 只是把它推到几乎必输。

**但结论不变，而且理由要说准**：`degraded_transitions=2` 就是判据 6 的失败条件本身。任何合理的 stale 阈值下，2% 的确认率都会让桥**反复进出 degraded**；一次 7 天的泡测会积累几十次 transition，而判据 6 要求它**在本次 run 内不增长**。所以不是「永远确认不了」，是「**确认率低到判据必然失败**」。

> ⚠️ 这个读数**会随时间变**（桥还在跑，canary 还在写库），所以它不是可复现的定值。可复现的是**形状**：`is_incoming=0` 那一栏远大于 `=1`，且 `lost` 远大于 `confirmed`。复跑：
> ```bash
> sqlite3 ~/.hyphae/messages.db \
>   "SELECT is_incoming, COUNT(*) FROM messages WHERE plaintext LIKE '%canary%' GROUP BY is_incoming"
> ```

> ### ⚠️ 2026-09-10 追加更正 —— 上面那组 2% 没有复现，而这一节的**因果**要改
>
> 上面的读数与推理**原文保留**，因为它是当时的真实观测；下面是后来量到的，两者不一致的地方以本段为准。
>
> **① 2% 没有复现，而且这一节自己在 40 行之前就预告过这件事。**
> 上面 `:149` 那段写着：那次读到 `sent=4 confirmed=3 lost=0`，若确认率真是 2%，4 次里 ≥3 次确认的概率是 `3.2×10⁻⁵`，**「所以那行背后有一个变量没被记下来」**。
> 那句话预测的正是这次发生的事 —— **文档在只有内部不一致、没有新实验的情况下，就正确判定了自己的一个读数不可信。** 而紧接着 `:160` 又写了「下面 2% 那组读数……条件明确，以它为准」：**同一份文档，一处说它背后有变量没记，一处说它条件明确。后者站不住，收回。**
>
> **② 那个上游竞态已经修了（本地补丁），而修完之后量不出差别。**
> 对照实验：同机、同一个本地 minirelay、同一套流程各跑一次，唯一变量是二进制打没打补丁（**未打补丁那个是从 git 还原源码后重编的，不是旧产物**）：
>
> ```
> 未打补丁  daemon 看到 10/30 → 10 条保住 is_incoming=1
> 已打补丁  daemon 看到 10/30 → 10 条保住 is_incoming=1
> ```
>
> 补丁本身是对的（单元测试打补丁前红、后绿，带对照），**但它不是这一节 ⛔ 的原因**。
>
> **③ 因果改判：⛔ 仍然成立，但依据换成 FU-43，不再是 FU-33 的竞态。**
> 新读数：本地 minirelay 上自发自收**只有约 1/3 到达 daemon**（三次试验都是 10/30，**稳定在 1/3 而不是随机 —— 稳定性本身是线索**）。按每条 canary 约 33% 的确认率，`staleAfterMs = 3×间隔` 下三次至少中一次约 70%，**不足以支撑 7 天无人值守**。
>
> **④ 所以这条 ⛔ 不因为补丁打了就失效。** 如果你读到这里的推论是「竞态修好了，可以用 minirelay 了」—— **那正是本段要挡住的推论。** ⛔ 的理由变了，指令没变。
>
> **⑤ 一个上面提出、这次被数据打掉的假设。** `:157` 猜「真正的决定变量可能是 daemon 那一刻有没有订阅同一条 relay」。这次实验里 **daemon 全程在线并订阅同一条 relay**，仍然只收到 1/3。所以那个假设**不是**唯一的决定变量。**FU-43 的候选原因（30 秒轮询的时序、minirelay 是否重放事件、发送速度快过订阅窗口）都还没验。**

**所以 relay 的选择不是「快 vs 慢」，是在两种失效之间选**：

| 选项 | 失效 |
|---|---|
| 公共 relay（damus 等） | 限流 → `degraded` 是 relay 的错 |
| 本地 minirelay | **竞态几乎必发 → 确认率约 2%,`degraded_transitions` 持续增长,判据 6 必挂** |
| `relay.aastar.io` | 当前下线 |
| **修上游** | 无失效 —— 见下 |

**唯一干净的解法是修上游**，而它是一行 SQL：`StoreOutgoingMessage` 的 upsert 让 `is_incoming` **单调**，即 `ON CONFLICT ... DO UPDATE SET is_incoming = messages.is_incoming OR excluded.is_incoming`（或者干脆先落库再发布）。FU-33 早就记了这条，今天它从「理论上可能」变成「实测挡住了 F5」。

**在上游修好之前**：用公共 relay，并把 `A24_NOSTR_CANARY_MS` 放大到 15 分钟以上以避开限流（相应放大 `A24_NOSTR_STALE_MS`）。这会降低活性探针的时间分辨率——**如实记下来**，别当成没有代价。

```bash
# ⛔ 别这么做 —— 见下一节:本地 relay 会让上游竞态 100% 触发,confirmed 永远是 0
# nohup ~/Dev/auraai/agent-speaker/bin/minirelay 7447 &
# A24_NOSTR_RELAY=ws://localhost:7447 ...

# 上游修好前的做法:公共 relay + 放大 canary 间隔避开限流
R=wss://relay.damus.io
hyphae daemon --identity agent24 --notify=false --relay "$R"
A24_NOSTR_RELAY="$R" A24_NOSTR_CANARY_MS=900000 A24_NOSTR_STALE_MS=2700000 \
  pnpm --filter @agent24/nostr-bridge bridge
```

**代价要如实说**：本地 relay **测不到真实网络路径**（DNS、TLS、跨机 WS、睡眠后的连接僵尸）。它测的是「桥 ↔ hyphae daemon ↔ relay 这套机制本身活不活」。真实网络那一维要等 `relay.aastar.io` 恢复后单独补一轮——**别把本地 relay 跑绿了当成"Nostr 通路全程活着"**。

### 🟡 模型运行时：没有它，判据 2 会挂

```bash
agent24 models        # 输出 "(no models — is a local LLM runtime running?)" 就是没有
curl -s http://127.0.0.1:8088/v1/models | head -c 200
```

起法（`omlx start` 需要 GUI 的 oMLX.app；headless 用 `serve`，2026-09-09 实测可用）：

```bash
nohup omlx serve --port 8088 --api-key xiaobao8088 --memory-guard safe > ~/.agent24/omlx.log 2>&1 &
agent24 models                                   # 要列出模型
agent24 chat "reply with the single word OK" --model Qwen3-0.6B-4bit   # 端到端 2.4s
```

泡测的定时任务要调模型。**连续失败 5 次，daemon 会把 schedule 置 `enabled=false, next_run_at=null`**——行还在，调度器已死，而判据 2 明确把 `auto_disabled` 判为失败。所以起跑前 oMLX（或 Ollama / LM Studio）必须在跑。

**且注意**：`agent24 service install` 捕获的 `EnvironmentVariables` **只有 `PATH`**（2026-09-09 实测）。`OMLX_URL` / `OMLX_API_KEY` 若非默认值，必须手工写进 plist，不能只 `export` 在 shell 里。默认值是 `http://127.0.0.1:8088` + key `xiaobao8088`（`agent24-models/src/router.rs` 的 `from_env`）。

---

## 在另一台机器上起跑（Mac mini 等）

整套是**可复制粘贴**的，除了微信扫码那一步。

```bash
# 0) 克隆 + 装工具链
git clone https://github.com/iDoris-ai/Agent24.git && cd Agent24
# 需要：rustc/cargo、pnpm、以及 hyphae 二进制（从 iDoris-ai/hyphae 构建：cd cmd/hyphae && go build）

# 1) 构建并安装（/usr/local/bin 若不可写才需要 sudo）
cd rust && cargo build --release -p agent24d -p agent24-cli && cd ..
cp rust/target/release/agent24 rust/target/release/agent24d /usr/local/bin/   # 两个都要

# 2) 模型运行时先起（见上面 🟡）
# 3) keystore 检查 + 建 identity（见上面 🔴）

# 4) 常驻
agent24 service install && agent24 service status
agent24 daemon status

# 5) Nostr 侧
nohup hyphae daemon --identity agent24 --notify=false > ~/.agent24/hyphae-daemon.log 2>&1 &
export A24_SPEAKER_BIN=$(which hyphae)
nohup env A24_SPEAKER_BIN=$A24_SPEAKER_BIN A24_NOSTR_IDENTITY=agent24 \
  pnpm --filter @agent24/nostr-bridge bridge > ~/.agent24/nostr-bridge.log 2>&1 &

# 6) 桥起来 2-3 分钟后，确认 canary 真的被 relay 拉回来了
cat ~/.agent24/nostr-bridge-health-agent24.json | python3 -m json.tool
#    要看到 state=ok 且 canaries.confirmed > 0。
#    confirmed 一直是 0 = 通路没通，这时**不要**继续放 7 天 ——
#    先查桥与 daemon 是否 watch 同一个 relay。

# 7) 微信（唯一需要人的一步）
pnpm --filter @agent24/wechat-bridge start   # 首跑打印二维码，用微信扫

# 8) 定时任务 + 冒烟 + 起跑
curl -s "http://127.0.0.1:$PORT/api/v1/schedules" -H "Authorization: Bearer $TOKEN"
scripts/soak-monitor.sh --interval 60 --duration 3600      # 先 1 小时冒烟
nohup scripts/soak-monitor.sh --log ~/agent24-soak.jsonl > ~/soak-monitor.out 2>&1 &
```

> **两台机器同时跑是可以的，但必须各用各的 Nostr identity。** 同一个 npub 被两个桥用，两边的 canary 会互相被对方「确认」——活性判据就失去意义了：它证明的变成「某个桥的通路活着」，不是「这个桥的通路活着」。第二台机器建 identity 时换个 nickname，并把 `A24_NOSTR_IDENTITY` 指过去。

---

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

**让它自己跑到点**（7 天到期自停）→ 监控打印 PASS / NEEDS REVIEW 摘要。中途 `Ctrl-C` / `kill` 得到的是 `RESULT: INCOMPLETE`（非 0 退出）——没跑满的 run 不是 F5 结论，采样到的一切当然都是健康的，那正是陷阱。所以只在确实要放弃这一轮时才中断。把摘要 + `~/agent24-soak.jsonl` 归档，据此在 `docs/specs/TASKS.md` 把 **F5** 标 done、**P1 收尾**。

> 远程支持：把 `~/soak-monitor.out` 的摘要行 / 异常行贴回来，我能帮你判读趋势、定位重启或泄漏根因。
