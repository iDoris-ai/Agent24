# F5 — daemon 稳定性泡测 Runbook

旧 `nostr-bridge` 已移除，见 [ADR-044 X7](adr/ADR-044-communication.md)。Nostr 身份由 `<data>/comm/hyphae-home` 管理；旧 F4b 开关、桥健康文件和 canary 判据已移除。入站自动执行将在 ADR-041a 准入里程碑重做。微信桥仅为[开发工具](../packages/wechat-bridge/README.md)，不属于产品泡测。

在笔记本上运行。默认 7 天、每 5 分钟采样；短时运行仅验证配置，不能替代完整 7 天验收。

## 判定标准

1. health 采样成功率 100%。
2. 每次采样至少存在一条日程，无 overdue、auto-disabled 或 schedules 读取错误。
3. 全程无人工重启或修复；人工核对日志与实际日程执行结果。
4. RSS 曲线平稳，不持续增长。

脚本只对前两条输出 PASS / NEEDS REVIEW，中断输出 INCOMPLETE。PASS 不证明 Nostr 通路或入站执行可用，也不替代人工确认。

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

```

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
```

## 收尾

**让它自己跑到点**（7 天到期自停）→ 监控打印 PASS / NEEDS REVIEW 摘要。中途 `Ctrl-C` / `kill` 得到的是 `RESULT: INCOMPLETE`（非 0 退出）——没跑满的 run 不是 F5 结论，采样到的一切当然都是健康的，那正是陷阱。所以只在确实要放弃这一轮时才中断。把摘要 + `~/agent24-soak.jsonl` 归档，据此在 `docs/specs/TASKS.md` 把 **F5** 标 done、**P1 收尾**。

> 远程支持：把 `~/soak-monitor.out` 的摘要行 / 异常行贴回来，我能帮你判读趋势、定位重启或泄漏根因。

## 历史实验记录（旧桥已移除，不作为当前启动说明）

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
| `relay.aastar.io` | 2026-09 时下线；2026-11-01 已恢复（Hyphae 自建），F5 需按新 relay 重测 |
| **修上游** | 无失效 —— 见下 |

**唯一干净的解法是修上游**，而它是一行 SQL：`StoreOutgoingMessage` 的 upsert 让 `is_incoming` **单调**，即 `ON CONFLICT ... DO UPDATE SET is_incoming = messages.is_incoming OR excluded.is_incoming`（或者干脆先落库再发布）。FU-33 早就记了这条，今天它从「理论上可能」变成「实测挡住了 F5」。
