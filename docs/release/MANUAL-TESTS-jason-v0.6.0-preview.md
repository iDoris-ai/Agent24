# v0.6.0-preview.20261028.1 —— jason 人工测试清单

> 发布后由 jason 在真机上做。脚本、CLI、HTTP 能验的都已由 Claude 在 10-27 跑过：一键脚本 PASS 39 / FAIL 0 / KNOWN 1 / SKIP 18，加上手工 X-07 PASS，合计 PASS 40；ID-8 用 CLI 在真机跑通。结果见 release PR #1421 和 `ACCEPTANCE-v0.6.0-preview.md`。这里只留**需要人眼看界面**和**需要断网**的项。
> 用例编号对应 `docs/release/ACCEPTANCE-v0.6.0-preview.md`。每项打 ✅ / ❌，❌ 时截图，贴到 release 的 issue，或直接发给 Claude。
> 安装包：GitHub Release `v0.6.0-preview.20261028.1` 的 `Agent24-0.6.0-preview.20261028.1-arm64.dmg`。装之前先 `shasum -a 256` 和 Release 页的 SHA256SUMS 比对。

## 0. 安装（X-06）—— 约 5 分钟
- [ ] 先备份：`cp -R ~/.agent24 ~/agent24-backup-1028`
- [ ] 用浏览器下载 dmg（这样会带 quarantine 标记，才是真实用户的路径），双击打开，拖进 Applications 覆盖旧版。
- [ ] 首次打开：只应弹出「从互联网下载」的常规确认，**不能出现**「无法验证开发者」或「已损坏」。
- [ ] 终端跑 `spctl -a -vv /Applications/Agent24.app`，应输出 `accepted` 和 `source=Notarized Developer ID`；再跑 `xcrun stapler validate /Applications/Agent24.app`，应输出 `worked`。
- [ ] 应用能启动；「记忆」页里旧记忆都在；Chat 页能发消息。
- [ ] 文档页：documents 模块没随包，看一下没装模块时页面显示什么（不报错、有提示就算过，见 G-D3）。

## 1. iDoris 断网实测（ID-8 的人工部分）—— 约 10 分钟
前置：今晚 Claude 已用 CLI 跑过 ID-8，用的是隔离 HOME、v0.3.1 归档、你的 oMLX 8088。这里只补「断网」和「界面」两部分。
- [ ] 按 ID-8 前置把 iDoris v0.3.1 起好。注意卡片保持 `on_demand`，不要用 resident（iDoris#582）。
- [ ] 桌面端 Settings 里填 `IDORIS_URL` 和 data key。
- [ ] **打开飞行模式**。
- [ ] Settings 选 `idoris/daily`，在 Chat 发一句话：应得到真实回复（Qwen3-0.6B 的风格，不是固定文本）。
- [ ] 界面上能看到：模型 `Qwen3-0.6B-4bit`、一个 Record-Id、locality=`loopback`。
- [ ] Settings 改选另一个 role 或模型后再发一次：显示的模型跟着变（ID-3 的界面部分）。
- [ ] 把 key 改错一个字符后发：提示指向 key（401/403）。
- [ ] 关掉飞行模式。

## 2. 记忆界面（M-14 / M-15）—— 约 5 分钟
- [ ] Chat 里说「记住我对花生过敏」：回执显示「已记住，可在「记忆」页查看或撤回」。
- [ ] 在「记忆」页暂停记忆后再说一句「记住…」：回执显示「记忆已暂停，这条没有被记住」；看完恢复。
- [ ] 「记忆」页：找到刚才那条 → 撤回 → 条目消失；再问「我对什么过敏」，不应再被召回。
- [ ] 已知问题，不算失败：普通轮次写入失败时，界面不显示「本轮未保存到记忆」（G-M1，下期修）。

## 3. 文档界面（D-12 / X-08，仅在手工装了 documents 模块时做）—— 约 10 分钟
- [ ] 文档页选一个 PDF 导入：有进度；导入中点「取消」能取消；失败后点「重试」能重试。
- [ ] 点开文档：详情和文字预览正常；关闭详情后焦点回到页面标题。
- [ ] 联合检查：导入 PDF → Chat 里「记住…」→「记忆」页撤回，全程不出现报错弹窗。

## 4. 桌面托管 daemon（W-03）—— 约 3 分钟
- [ ] 全新数据目录启动桌面端（可以先把 `~/.agent24` 改名再启动，测完改回来）：Chat 能发、Runs 页能列出。
- [ ] 托盘点「停止」只停桌面自己拉起的 daemon。
- [ ] 退出后 `daemon.json` 被清理。
- [ ] 桌面开着时，CLI 连不上它拉起的 daemon，这是预期（提示里写了 `NO_DAEMON`）。

## 不用测（已知阻塞或无入口）
- K-03（模块授予卡片）：授予入口默认关闭，没有协议 4 的示例模块（K-02 阻塞）。
- W-04（Knowledge host 通道）：Provider 还没接入，没有用户入口，由单测覆盖。
- ID-5 a/b/c 的预算推荐比对：推荐结果本期不经 HTTP 暴露。

## 测完
在 Release 对应的 issue 下回一句「人工测试：全部 ✅」，或者列出 ❌ 项。Claude 按结果更新验收文档；需要修的开 PR。
