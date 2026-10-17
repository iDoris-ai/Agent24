# Agent24 v0.5.0 桌面端真机验收（2026-09-30）

> 代码基线：Agent24 `main@f46194a9c2773eedb37081574a04062e91de3867`（`chore(release): v0.5.0` #589），working tree clean，未做任何代码改动。
> 本次验收**只验证、截图、记录，不修代码**。

## 1. 环境

| 项 | 值 |
|---|---|
| 构建 | `cargo build --release --bin agent24 --bin agent24d`（`rust/target/release/`，已是最新，构建于验收当天 21:35）；`apps/desktop` 下 `pnpm run build`（`vite build && tsc`，生成 `apps/desktop/dist/`） |
| 桌面端运行模式 | **生产包**：`NODE_ENV` 未设为 `development`，`win.loadFile(dist/renderer/index.html)`；devtools CDP 页面 URL 确认为 `file:///…/apps/desktop/dist/renderer/index.html`，不是 `localhost:5173`（FU-91 已修复，`VITE_DEV_SERVER_URL` 可配置，默认路径未触发） |
| 隔离 HOME | `/tmp/a24v05-desk`（短路径，不是 `~/.agent24`），独立 `--user-data-dir=/tmp/a24v05-desk-profile` |
| 后端二进制 | `AGENT24D_BIN=/Users/jason/Dev/auraai/Agent24/rust/target/release/agent24d`（release，非 debug）；由 `apps/desktop` 的 `BackendManager` 以 `agent24d serve --port 0` 拉起，随机端口，本次为 **57555** |
| 已安装模块 | Sin90 `v0.5.0`（`github.com/iDoris-ai/Sin90` release `sin90-0.5.0-macos-arm64.tar.gz`，SHA256 校验通过：`a0b9630a…`）；Cos72 `v0.1.0`（`github.com/MushroomDAO/Cos72` release `cos72-0.1.0-macos-arm64.tar.gz`，SHA256 校验通过：`148547dd…`）；均用 `agent24 os install <dir>` 装进隔离 HOME |
| 本机 oMLX | 运行中，`127.0.0.1:8088`，18 个模型（`Qwen3-8B-4bit`、`Qwen3-0.6B-4bit` 等对话模型 + FLUX/OCR/CosyVoice 等非对话模型），API Key `xiaobao8088`（未启动/未下载任何新模型，纯观察） |
| CDP 驱动 | Electron 以 `--remote-debugging-port=9333` 启动；自写 Node 脚本用仓库自带的 `apps/desktop/node_modules/ws` 直连 CDP WebSocket，做 `Runtime.evaluate` / `Page.captureScreenshot` / `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent`（未装 Playwright，未碰 `pnpm-lock.yaml`） |
| Electron / Node | Electron `34.5.8`（仓库自带 `node_modules`，未跑 `install.js`，二进制本来就在）；Node `v24.21.0` |

## 2. 结论总表

| # | 菜单 | 结论 | 一句话原因 |
|---|---|---|---|
| 1 | 对话 | ✅ 可用 | 发消息→收到 Qwen3-8B-4bit 本地回复，延迟 29,899ms 正确显示，模型选择正确（排除了生图/OCR 模型，FU-89 已修复） |
| 2 | 工作台 | ⬜ 空壳/占位 | 9 张能力卡片全部硬编码在 `Workbench.tsx`，**没有任何 `onClick`**；点击标"✓ 可用"的翻译卡片也无任何反应（无导航、无网络请求、无 console 输出），标签具有误导性 |
| 3 | 任务 | ✅ 可用（空数据） | 真实轮询 `/api/v1/runs`（3 秒刷新），当前会话没有触发过 agent 自主运行，`{"runs":[]}` 与 UI 一致，属于真实空状态而非假壳 |
| 4 | 调度 | ✅ 可用 | 手动经 Sin90 REST 建了一条 routine，桌面端立刻显示"来自模块 sin90"；点「立即运行」→ 绿色提示「已投递给模块（fire_id …）」，与内核 `/api/v1/schedules` 状态一致 |
| 5 | 审批 | ✅ 可用（空数据，未覆盖到操作路径） | 真实轮询 `/api/v1/approvals`（2 秒刷新），当前为 0 条，与直接 curl 一致；本次未能在验收范围内构造出一条真实的模块 gate 审批（触发条件在 `agent24-policy::guardian`，属于工具调用风险升级路径，未在桌面聊天里观察到工具调用），因此**未验证到「操作」这一步**，是覆盖缺口而非产品缺陷 |
| 6 | 语音 | ✅ 可用（正确的未附着态） | 无 AgentEar 时清楚显示「附着状态：未附着（尚未配对）」，输入框和「让它说」按钮均被正确禁用（`disabled:true`），不会误导用户去发一个注定失败的请求 |
| 7 | 模型 | ❌ 不可用（有 bug） | 页面显示「oMLX 未运行」，但 oMLX 明明在跑（Chat/Settings 两页都证明了）；根因是 `Models.tsx` 调用了 `GET /api/llm/models`（404），而内核真实路由是 `/api/v1/models` — 见「发现的问题」#1 |
| 8 | 模块管理 | ✅ 可用 | 「OS 模块（内核挂载）」区块正确列出 cos72、sin90 的挂载路径、版本、grants，「已挂载/停用」按钮存在（FU-90 已修复，此前只能管 npm 社区模块） |
| 9 | 设置 | ⚠️ 部分可用 | AI Model Service 区块真实可用（Connected 状态、18 model(s)、模型 Resident/On-demand 分类经 localStorage 真实持久化）；但 **Daemon Port 输入框 / Language 下拉 / Save Settings / Reset Defaults 四个控件全部没有 `onChange`/`onClick`**，纯装饰，见「发现的问题」#2 |
| 公共 | 顶栏当前模型 | ✅ 正确 | 对话页右上角显示 `Qwen3-8B-4bit · Agent24 默认`，是对话模型而非生图/OCR 模型 |
| 公共 | 侧栏底部后端端口 | ✅ 正确（FU-93 已修复） | 全程显示「后端服务运行中 :57555」，与实际 daemon 端口（`daemon.json` 里的 57555）一致，此前 2026-09-27 演示记录的"显示 :8765 实际 60128"问题已不复现 |
| 公共 | 主题切换 | ✅ 可用 | 右上角 🌙/☀️ 点击后深浅色立即切换，无报错 |

## 3. 截图（按菜单顺序）

1. `01-chat-initial.png` — 对话首页，默认模型 `Qwen3-8B-4bit`
2. `01-chat-sending.png` — 发送消息后「思考中…」
3. `01-chat-response.png` — 收到回复，耗时 `29,899 ms`
4. `02-workbench.png` — 工作台 9 张能力卡片，仅「翻译」标"✓ 可用"
5. `02-workbench-translate-open.png` — 点击「翻译」卡片后无任何反应（仅 focus 边框）
6. `03-runs.png` — 运行任务页，真实空状态
7. `04-schedules.png` — 调度页，安装 Sin90 后尚未创建 routine 时为空
8. `04-schedules-with-routine.png` — 经 Sin90 REST 建 routine 后，桌面端显示「来自模块 sin90」
9. `04-schedules-after-run-now.png` — 点「立即运行」后绿色提示「已投递给模块（fire_id …）」
10. `05-approvals.png` — 待审批 (0)，真实轮询空状态
11. `06-voice.png` — 语音页，「附着状态：未附着（尚未配对）」
12. `06-voice-after-say.png` — 未附着状态下输入框/「让它说」按钮均被禁用
13. `07-models.png` — 模型管理页，**错误显示**「oMLX 未运行」
14. `08-modules.png` — 模块管理页，cos72、sin90 均显示「已挂载」+ grants
15. `09-settings.png` — 设置页顶部，AI Model Service 显示 Connected · 18 model(s)
16. `09-settings-scrolled.png` — 设置页下半部分，Daemon Port / Language / Save 按钮
17. `09-settings-after-save.png` — 修改 Language 为 English 并点 Save 后，UI 文案未变
18. `10-theme-toggle.png` — 浅色主题下的设置页（验证主题切换 + Qwen3-8B-4bit 分类已存为 Resident）

## 4. 发现的问题

| 编号 | 问题 | 影响 | 建议优先级 |
|---|---|---|---|
| AUDIT-1 | 「模型管理」页（`apps/desktop/src/renderer/pages/Models.tsx:33`）调用 `GET /api/llm/models`，但 agent24d 内核真实路由是 `/api/v1/models`（`rust/apps/agent24d/src/server.rs:756`）。请求恒 404，页面因此**始终**显示「oMLX 未运行 — 启动后此处显示实时模型状态」，即使 oMLX 正常运行且其他页面（对话、设置）都能证明模型可用。 | 用户在「模型」页看到的实时状态完全是假的，可能误判 oMLX 没起来去手动重启、重装，或怀疑对话功能不可靠；与「设置」页（走正确的 `omlxModels` IPC）的状态矛盾，体验不一致。 | 高——一行路径改动即可修复，且直接影响一个核心菜单的可信度 |
| AUDIT-2 | 「设置」页（`apps/desktop/src/renderer/pages/Settings.tsx`）的 `Daemon Port` 输入框（357 行附近，`<input defaultValue="8765" />`）、`Language` 下拉（`<select defaultValue="zh">`）、`Save Settings` 按钮、`Reset Defaults` 按钮**四个控件全部没有绑定任何状态或事件处理器**——纯静态 HTML，点击/修改后不发生任何持久化，重新进入该页会全部复位为默认值。只有页面上半部分「AI Model Service」（连接状态、Service URL、API Key、模型 Resident/On-demand 分类）是真实可用、经 localStorage 持久化的。 | 用户修改语言或守护进程端口后点「保存」，看起来成功（按钮存在、没报错）但其实什么都没发生，是一个「看起来能用实际不能用」的陷阱；`Daemon Port` 显示恒为 8765，与实际运行端口（本次 57555）不符，可能被当作"配置就是这个端口"的错误依据。 | 高——属于典型的"假交互"，用户无法通过任何方式发现保存失败（无报错、无提示） |
| AUDIT-3 | 「工作台」页（`apps/desktop/src/renderer/pages/Workbench.tsx`）9 张能力卡片全部来自硬编码数组，**没有一个卡片绑定 `onClick`**，包括唯一标注"✓ 可用"的「翻译」卡片；点击只触发 CSS `cursor:pointer` 的视觉反馈（黄色 focus 边框），不发起任何请求、不导航到任何功能页。 | "✓ 可用"标签具有误导性——用户会认为点击后能直接执行翻译，实际这整页目前是纯展示/路线图性质。 | 中——如果工作台本来就是"能力预览"页不需要可点，建议去掉"✓ 可用"这个强承诺性文案，或改成"即将开放入口"之类更准确的表述；如果计划让翻译可点，需要补交互 |
| AUDIT-4 | 「审批」页本次验收未能覆盖"实际有一条待审批记录时能否操作（通过/拒绝）"这条路径。已确认列表页正确轮询真实后端（`/api/v1/approvals`，与 curl 结果一致），但触发一条真实的模块 gate 审批依赖 `agent24-policy::guardian` 的工具调用风险升级流程，桌面聊天页在本次验收中没有观察到任何工具调用（只是纯 LLM 对话），因此没能产出真实审批记录来验证按钮。 | 不是已确认的产品缺陷，而是本次验收的覆盖缺口；如果桌面聊天当前版本确实不支持带工具调用的 agent 循环，审批页的"操作"路径事实上可能长期没有真实数据可测。 | 低（跟踪）——建议下次验收前找到一条能稳定触发 gate 审批的路径（例如某个需要 `AlwaysReview` 的工具类型），或者从模块侧提供一个测试端点 |

## 5. 未覆盖 / 已知限制

- AgentEar 硬件未接入，语音页的实际 ASR/TTS 收发链路未做端到端验证（只验证了"未附着"时的降级表现，这也是任务要求的验收点）。
- 「任务」页和「工作台」页没有触发出真实的 agent 自主运行（`/api/v1/runs` 全程为空），因为本次验收的对话/调度动作都不落在这个概念上；两页展示的都是真实、一致的空状态，不是假数据。
- 未测试模块「停用/启用」「卸载」等破坏性操作（任务要求只做验证记录，不做会改变环境状态且难以复原的操作）。

## 6. 进程清理确认

结束时执行：
- `pkill` 停止本次启动的 Electron 主进程及其 Renderer/GPU/Helper 子进程（按 `--user-data-dir=/tmp/a24v05-desk-profile` 命令行特征匹配）
- 确认其拉起的 sidecar `agent24d`（pid 随 Electron 子进程退出而终止，`HOME=/tmp/a24v05-desk`）随之退出
- `ps aux | grep -E "agent24d|Electron"` 复核，验收结束时台面干净，无本次会话残留进程
- 未清理 `/tmp/a24v05-desk`、`/tmp/a24v05-desk-profile`、`/tmp/a24v05-work` 等临时数据目录本身（如需彻底清理可另行删除，均在 `/tmp` 下，与用户真实 `~/.agent24` 无关）
