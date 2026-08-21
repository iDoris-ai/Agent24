# F5 — 7×24 稳定性泡测 Runbook

> **F5 是物理任务**：Mac mini 连续 **7 天**、定时工作流照跑、**无人工干预**。
> 代码侧全部就绪（F1a 开机自启 · F2 崩溃自愈 · F3 微信 · F4 Nostr），此文把「按下起跑」变成一条可照做的清单。这份 runbook + `scripts/soak-monitor.sh` 是能替你做的最大块；7 天的时钟只能在你的机器上走。

## 判定标准（跑完对照）

一次通过的泡测应满足：

1. **可用性**：`soak-monitor` 记录的 health 命中率 **100%**（偶发 daemon 重启允许——那正是 F2 要证明的——只要 health 每次都恢复）。
2. **调度不卡**：任何采样点都没有 `next_run_at` 落在过去仍未触发的 schedule（`overdue == 0`）。
3. **无人工干预**：7 天里你没有手动重启 daemon / 重连渠道 / 清状态。
4. **无内存泄漏**：`rss_mb` 曲线平稳，不单调爬升。
5. **渠道存活**：微信 / Nostr 入站在第 1 天和第 7 天都能触发 run + 审批回。

`soak-monitor.sh` 退出时按 1、2 自动给 PASS / NEEDS REVIEW；3–5 靠你日常抽查 + 日志。

## 一次性准备

```bash
# 1) release 构建（泡测用 release，别用 debug）
cd rust && cargo build --release -p agent24d -p agent24-cli
sudo cp target/release/agent24 /usr/local/bin/   # 或加进 PATH

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
```

## 收尾

`Ctrl-C`（或 7 天到点自停）→ 监控打印 PASS / NEEDS REVIEW 摘要。把摘要 + `~/agent24-soak.jsonl` 归档，据此在 `docs/specs/TASKS.md` 把 **F5** 标 done、**P1 收尾**。

> 远程支持：把 `~/soak-monitor.out` 的摘要行 / 异常行贴回来，我能帮你判读趋势、定位重启或泄漏根因。
