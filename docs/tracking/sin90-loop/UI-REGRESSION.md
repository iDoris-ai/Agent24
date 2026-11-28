# Sin90 桌面闭环回归记录（2026-11-24）

范围：本工作树 `ab/sin90-loop-01-ui`。没有提交、推送、改 pin，也没有修改 `../Sin90-loop-core`。计划末尾的今天页保真条款优先于 HTML 预览的默认布局。

## 已实现

- 今天默认进入旧工作台；新增「今日概览｜四象限」，快速捕捉草稿在两视图及方向/节奏之间保留。概览继续使用原 must_do 顺序、专注、任务更多操作、收件箱及归类提案、本周节奏和方向速览。
- 四象限借鉴下载预览的两列、四色和文字标签；650px 以下单列。使用 `/tasks` 的现有任务，按 task_id 去重，不创建另一套任务。四象限之外单列真实「未评估」。
- AI 判断和人工依据分开展示；可重评、人工改判和清除改判。旧模块缺少优先级能力时禁用相关写操作，完成任务及原工作台仍可使用。
- 完成仍通过现有合法状态转换，到 done 后可在本周完成素材补记。素材读取 weekly draft 的 completed_tasks，展示完成时分类和备注；没有备注明确表示无法推断成效，不从当前任务分类倒推历史。
- 复盘页增加开始本周复盘、生成复盘建议和正文展示，沿用现有 reviews、ai/summarize 和提案采纳入口。浏览素材和生成建议不直接改计划。
- 新写操作均经主进程校验并附人类钥匙；通用 backendProxy 只放行新增 GET。

## 接口核对

依据 `../Sin90-loop-core/SIN90-LOOP-CONTRACT.md` 及当前 `src/http/task_loop.rs`、`src/store/task_loop.rs`，只读查看。

- 契约已更正完成操作为 `PATCH /tasks/{task_id}`，与现有桌面一致。
- 当前后端的 human_override/effective 仅含 quadrant、rationale；桌面接受这个简版，也接受契约描述的完整评估形状。AI 评估仍校验 importance、urgency、evaluated_at。
- 新模块返回显式未评估对象；旧模块缺端点或缺 completed_tasks 时按能力降级。没有伪造默认 AI 判断。

## 自动回归

所有测试使用进程内独立 fixture，未访问用户的 Sin90 数据目录。闭环测试验证桌面请求及界面行为；它不能代替真实后端的事件去重和模型推理验收。

先验证失败：增加的概览/四象限切换测试在实现前因找不到四象限 tab 失败；手动周复盘入口测试在实现前因找不到「开始本周复盘」失败。日志：`/tmp/sin90-red.log`、`/tmp/sin90-review-red.log`。

| 检查 | 结果 |
| --- | --- |
| `pnpm install --frozen-lockfile` | 通过，锁文件未变 |
| `pnpm typecheck` | 通过 |
| `pnpm lint` | 通过，15 个既有 warning、0 error |
| `pnpm build` | 通过，有既有大 chunk 提示 |
| `Sin90Page.test.tsx` | 29 项通过 |
| `sin90-write.test.ts` | 12 项通过 |
| `loop-model.test.ts` | 4 项通过 |
| `backend-proxy.test.ts -t '^A1 '` | 88 项通过，18 项未选择 |

主流程覆盖：概览→四象限→概览、捕捉草稿、方向/节奏往返、原任务更多操作、开始专注、归类提案采纳、旧模块禁用态、跨视图完成状态一致、人工改判→重评仍保留→清除、无备注完成→补记→本周同一条素材→开始复盘→生成建议→显式采纳。

日志：`/tmp/sin90-page-final.log`、`/tmp/sin90-final-focused.log`、`/tmp/sin90-model.log`、`/tmp/sin90-allowlist-final.log`、`/tmp/sin90-typecheck-final.log`、`/tmp/sin90-lint-final.log`、`/tmp/sin90-build-final.log`。

## 首轮沙箱运行记录

`pnpm test` 已尝试；data-dir 80 项通过。desktop 多个既有网络/进程测试失败，包含 backend-proxy、knowledge-view、knowledge-host、backend-manager、presence-connection、service-connection 等：本沙箱禁止监听，出现 `listen EPERM 127.0.0.1`，并伴随后续超时。creative-serve-web 的已有 H1 用例也失败。批量超时后停止全量运行，没有完整汇总，不记作门禁通过。全量日志 `/tmp/sin90-test.log`。

首次全量运行还发现本次任务列表 URL 的嵌套模板写法不符合既有路由静态扫描测试；已改为明确的两个 URL 分支，修复后 A1 全部通过。

为目视检查制作了 `/tmp/sin90-ui-preview` 的离线 fixture 构建；Chromium 启动后退出（Target closed），没有获得有效截图，因此未声称桌面宽窄屏目视验收通过。

## 独立复验

重新构建当前源码的离线 fixture，在 Chromium 和 Electron 窗口中切换概览与四象限。默认概览显示专注、三件事、收件箱和节奏。切换后快速捕捉草稿保留。1100px 时四象限两栏，390px 时单栏且无横向溢出。

在包括同方向桌面测试修复的工作树运行 pnpm install --frozen-lockfile、pnpm typecheck、pnpm lint 和 pnpm test，均成功。desktop 共 4374 项通过、8 项跳过，lint 为 0 error、15 warning。真实 Sin90 HTTP 与提案确认在领域模块单独验收。桌面使用隔离 fixture；已安装客户端与外部模型服务的整机联调尚未执行。
