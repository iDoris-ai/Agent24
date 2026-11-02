索引：[架构总入口](../architecture/README.md) · [ADR 索引](../adr/README.md)

# Open Design iDoris 版改造：调研与执行计划

调研日：2026-11-02。只读对象是 fork `/Users/jason/Dev/auraai/open-design-agent24`（`origin` = `iDoris-ai/open-design-agent24`，`upstream` = `nexu-io/open-design`）。当时检出分支 `integration/agent24-open-design-v0.22.2`，HEAD `34bc2ba810cf894daff6a7c1736d1cef6c7726b1`（2026-10-06，`git describe` = `open-design-v0.22.2-23-g34bc2ba810`）。本文不改那个仓库。

Agent24 侧对照的是主 checkout 里仍钉着这颗 SHA 的桌面集成（`apps/desktop`）。嵌入方式在本 worktree 与主 checkout 一致。

## 结论

1. 先把 fork 升到 `open-design-v0.26.1`（2026-10-22），并只带回 Agent24 运行时、headless 启动器和 `resourceSafeBase` 安全补丁。首页品牌先让给上游新 Home，小人、改名放到最后一片。文本冲突只有 4 个文件，风险中等，卡点是 `sidecars.ts` 和 Windows 打包，不是 2000 个文件一起爆。
2. 右上角 GitHub 星标和积分按钮都在 `EntryNavRail.tsx` 的 `EntryTopRightCluster`。删按钮是小改动；OD 云账户本身缠进协作和计费，最小切法是加本地单用户桩并把云端口关掉，而不是一次删掉一百多个文件。
3. 深浅色对不齐，是因为 OD 把 `data-theme` 写死成 `light`（v0.26.1 仍如此），用来挡住系统深色。Agent24 的深色是外壳上的 CSS class，进不了旁边的 `WebContentsView`。要在 OD 里接受宿主传入的主题，再由 Electron 在切换时写进去。
4. OD 已有 19 种语言，含简中、繁中和泰语。Agent24 嵌入时没有装 OD 的桌面 host bridge，语言不会跟着外壳走。fork 自己的文案应做成覆盖层，不要再改上游那 19 份大字典。
5. 许可证是 Apache-2.0。可以改产品名，但商标条款不授权把成品叫做官方 Open Design。建议界面对外写「iDoris Design」，旁边注明「基于 Open Design」。

## 现状

### 版本

| 点 | 值 |
|---|---|
| fork HEAD | `34bc2ba810`，分支 `integration/agent24-open-design-v0.22.2` |
| fork 基线 tag | `open-design-v0.22.2` = `73953213a6`，2026-09-10，发布说明标题 *More reliable update restarts* |
| 上游目标 | `open-design-v0.26.1` = `ac476788b7`，2026-10-22 10:51 UTC，GitHub Release *Intelligent Router* |
| 我们相对基线的提交 | `git log --oneline open-design-v0.22.2..HEAD` 为 **23** 行（含 6 个 merge） |
| 本地 `git log --oneline open-design-v0.22.2..open-design-v0.26.1 \| wc -l` | **131** |
| GitHub compare `open-design-v0.22.2...open-design-v0.26.1` | `status=diverged`，`ahead_by=236`，`behind_by=19` |

131 不是完整跨度。本 clone 是浅克隆（`.git/shallow` 两条边界：`73953213a6` 即 v0.22.2 本身，以及 `f5707c8cae` = `open-design-v0.24.0~12`，也是 `origin/main` 的浅根）。`git merge-base open-design-v0.22.2 open-design-v0.26.1` 因此失败。v0.22.2 的父提交 `43a82d605f`（*prepare v0.22.2 update reliability notes*）在对象库里，但不是 v0.26.1 的祖先。GitHub 的 236 / 19 才是两条发布线的真实分叉：v0.26.1 比共同祖先多 236 个提交，v0.22.2 这条 release 线有 19 个提交没有进入 v0.26.1。

两棵树可以直接比，不依赖那条断掉的历史：`git diff --shortstat open-design-v0.22.2 open-design-v0.26.1` 为 **2311 files, +268407 / −48503**。GitHub compare 的文件列表在 300 个封顶，以本地树 diff 为准。

中间发布（GitHub Release，日期为 published_at）：

| tag | 发布 | 代号 | 和本改造有关的点 |
|---|---|---|---|
| v0.23.0 | 2026-09-20 | Home, Rebuilt | Home、最近项目、对话收成一条路径；空余额在离开 Home 前就被拦住 |
| v0.24.0 | 2026-09-22 | Every Run, Kept Intact | 运行中断后还能接上；预览不再被盖成空白 |
| v0.24.1 | 2026-09-24 | World's First Design Plan | 全屏退出按钮补了深浅色；macOS headless 可以原地变成桌面窗口（#8398） |
| v0.25.0 | 2026-10-15 | OpenDesign Motion | Motion / MP4；登录后默认进 Hosted（#8567）；积分不足时 Home 草稿不发出去；另一台设备的云同步要用户点头（#8683） |
| v0.26.0 | 2026-10-20 | Share, Review, Refine | 公开分享链接、评论同步回项目、把评论交给 Agent（#8408）；BYOK 可填自定义模型 |
| v0.26.1 | 2026-10-22 | Intelligent Router | Skill Discovery（#8804）；Home 发送后直接进入项目（#8267）；注册过的 runtime 和 CLI BYOK（#8702） |

没有单独的「破坏协议」声明。对 Agent24 真正会撞上的行为变化是：Home 被重写（v0.23 起，v0.26.1 的 hero 不再挂大 logo）、分享和计费更深地绑在 OD 云上、打包侧多了 `finalizeRuntimeManifest`。仓库根没有 `NOTICE` 文件。`CHANGELOG.md` 在 v0.26.1 仍停在 0.9.0 时代；0.24 之后的说明在 GitHub Release 和 `docs/CHANGELOG/v0.24.0`、`v0.24.1`、`v0.26.0`。v0.23 / v0.25 / v0.26.1 没有对应的 `docs/CHANGELOG` 目录，以 Release body 为准。

### 23 个补丁

`git log --oneline open-design-v0.22.2..HEAD`。6 个是 merge，功能在下面 17 个里。

**Agent24 集成（daemon runtime）**

- `227aa45d01` feat(agent24): add ACP runtime adapter。新增 `apps/daemon/src/runtimes/defs/agent24.ts`：`id: 'agent24'`，`buildArgs: () => ['acp']`，`streamFormat: 'acp-json-rpc'`。
- `ff27c8f24d` 持久化 ACP 文本产物。
- `cee4bcd915` 重复 ACP 产物改为更新而不是再插一条。
- `002126b547` 长可见产物不要被截断。
- `83ce740f4a` 把 `agent24` 登记进 analytics 的 provider 枚举（`packages/contracts`）。

**打包 / headless 启动器**

- `c696ea2992` 增加 `apps/packaged/src/agent24-headless.ts`。
- `deff6636a7` 把启动器物化进 prebundled。
- `c503b6aec9` 收口启动器的 runtime bundle。打包入口在 `tools/pack/src/resources/index.ts` 的 `copyAgent24HeadlessLauncher`，由 `tools/pack/src/mac/app.ts`、`linux.ts`、`win/app.ts` 调用。

**安全（resource trust root / resourceSafeBase v2）**

- `fb20aa2bb8` 打包资源的信任根和安装目录分开。
- `813322cab2` headless 协议 v2：显式 `resourceSafeBase`。
- `3d2df0991f`、`9961f2f7cc`、`c192732b2a`、`ed60d6d237` 三轮复审，含 TOCTOU 快照。Agent24 侧 `creative-serve-web.ts` 已按协议 2 传这个字段。

**品牌**

- `c49ba6a301` 首页大 logo 换成 `IDorisDesignLogo`（卡通小画家）。
- `88ec4b3885` 标题、加载文案、17 个 locale 的 `app.brand` 改成 `iDoris Design`。**没改** `zh-CN.ts` / `zh-TW.ts`：简中仍是 `app.brand: "OpenDesign"`，副标题仍是「由 Nexu Labs 出品」（`apps/web/src/i18n/locales/zh-CN.ts` 约 295–297 行）。

**文档**

- `17c4e86f21` `docs/agent24-baseline.md`，记下 v0.22.2 基线。

相对基线的 diff：46 个文件，+2198 / −46。

### Agent24 怎么嵌 OD

桌面主进程 `apps/desktop/src/main/creative-serve-web.ts` 拉起 fork 的 `agent24-headless.cjs`（协议常量 `AGENT24_HEADLESS_PROTOCOL = 2`），钉死 `OPEN_DESIGN_PIN_VERSION = '0.22.2'`。就绪后 origin 必须是 `http://127.0.0.1:<port>`，不能带路径、query、账号。

`apps/desktop/src/main/main.ts` 用另一块 `WebContentsView`（`partition: persist:agent24-creative`，sandbox，无 node）`loadURL(status.origin)`。它是主窗口的兄弟 view，不是渲染进程 DOM 里的 iframe，所以 Agent24 的 CSS class 影响不到 OD。外链用系统浏览器打开。

组件按需下载，不打进安装包。SHA 和版本钉在：

- `apps/desktop/scripts/pack-open-design-component.mjs`：`OPEN_DESIGN_PIN_VERSION = '0.22.2'`，`OPEN_DESIGN_SHA = '34bc2ba810cf894daff6a7c1736d1cef6c7726b1'`
- `apps/desktop/scripts/stage-open-design-resources.mjs`、`prepare-open-design-linux-resources.mjs` 各写一份同样的 pin
- `verify-mac-open-design.sh` 从 pack 脚本读这两个值
- `open-design-mac-release.mjs` 的签名白名单：`open-design/bin/libexec/opencode/opencode`

v0.26.1 里这条路径还在：`tools/pack/src/vela-cli.ts` 的 `OPEN_CODE_COMPANION_RELATIVE_PATH = ["libexec", "opencode"]`，校验读 `open-design/bin/libexec/opencode/opencode`（`tools/pack/src/resources/runtime-verification.ts`）。签名白名单不用因为路径改名而改。上游另外新增了自己的 `apps/packaged/src/headless.ts`、`headless-runtime.ts`、`managed-headless.ts`，和我们的 `agent24-headless.ts` 不是同一个文件。

## 逐项方案

### A. 升级到 v0.26.1

**做什么。** 在 fork 上新开集成分支（建议 `integration/agent24-open-design-v0.26.1`），以 v0.26.1 为底，把运行时、启动器、`resourceSafeBase` 重新放上去。品牌和首页 logo 这一轮主动采用上游，避免在重写过的 Home 上硬合。Agent24 把 pin 版本和 SHA 换成新 HEAD，跑现有 Creative 启动和 macOS 资源校验。

本地不能 `git rebase`，因为浅克隆没有共同祖先。评估用的是只读三方树合并（旧版 `git merge-tree <v0.22.2 tree> <HEAD tree> <v0.26.1 tree>`），不碰工作区。`git cherry -v open-design-v0.26.1 HEAD` 里我们的提交全是 `+`，上游没有等价补丁。

两边都改过的文件 33 个。出现冲突标记（`<<<<<<<`）的只有 4 处：

| 文件 | 冲突在哪 |
|---|---|
| `apps/packaged/src/sidecars.ts` | 我们把 `resourceSafeBase` 传进 `createPackagedDaemonManagedPathEnv`；上游在同一段加上 `OD_APP_SESSION_ID` / `OD_LAUNCH_ATTEMPT_ID`，并新增分享 viewer 的环境变量。两段都要留 |
| `apps/web/src/components/HomeHero.tsx` | 我们 import `IDorisDesignLogo`；上游改成 `RotatingTitleWord`，hero 的 JSX 整段重写（dock、空态折叠）。不要自动合 |
| `apps/web/tests/components/home-logo-assets.test.ts` | 我们断言挂了 `IDorisDesignLogo`；上游断言不再挂 `PixelScanLogo`、改挂 `RotatingTitleWord` |
| `tools/pack/src/win/app.ts` | 我们调用 `validateWinPackagedAppRuntime(appRoot, usePrebundle)`；上游改成先 `finalizeRuntimeManifest(appRoot)`，校验函数少了一个参数 |

其余 29 个「两边都改」没有冲突标记，包括 `apps/daemon/src/server.ts`、`runtimes/auth.ts`、`runtimes/types.ts`、`apps/packaged/package.json`、`sidecars.test.ts`、`client-app.tsx`、`layout.tsx`、17 个 locale（我们每个只改了 `app.brand` 一行；`zh-CN` / `zh-TW` 不在其中）、`home-hero.css`、analytics 两个枚举文件、`tools/pack/src/mac/app.ts`、`tools/pack/tests/resources/resources.test.ts`。mac 打包能自动合上，仍要看 `copyAgent24HeadlessLauncher` 有没有被 `finalizeRuntimeManifest` 盖掉。`linux.ts` 和 `tools/pack/src/resources/index.ts` 上游没改这些行，补丁可以干净套上。我们新增的 `agent24.ts`、`agent24-headless.ts` 在 v0.26.1 不存在（`added in both` = 0）。

**OD fork 改哪些。** 重放：`apps/daemon/src/runtimes/defs/agent24.ts` 及 ACP 测试、`apps/daemon/src/runtimes/registry.ts`、`plain-stream.ts`、`apps/packaged/src/agent24-headless.ts` 与测试、`esbuild.config.mjs`、`tools/pack/src/resources/index.ts`、mac/linux/win 的 copy 调用、`sidecars.ts` 里 v2 信任根。手合 `sidecars.ts` 和 `win/app.ts`。`HomeHero.tsx`、`IDorisDesignLogo.tsx`、locale 里的品牌字符串这一片先用上游。

**Agent24 改哪些。** 新 SHA 写进 `pack-open-design-component.mjs`、`stage-open-design-resources.mjs`、`prepare-open-design-linux-resources.mjs`、`creative-serve-web.ts` 的 `OPEN_DESIGN_PIN_VERSION`。确认 `open-design-mac-release.mjs` 的 opencode 白名单字符串不用改。`creative-serve-web.test.ts` 和 `open-design-mac-release.test.mjs` 跟 pin 走，改常量即可。

**风险。** 自动合上的 `server.ts` / `auth.ts` 可能语义错了但没有冲突标记，必须跑 daemon 里 agent24 runtime 测试和 packaged sidecar 测试。`resourceSafeBase` 的路径比较若被上游 session env 重构绕开，信任根会失效，这是安全补丁，不能只看编译通过。v0.24.1 的「headless 原地变桌面窗口」不要接进 Agent24：我们的 view 由 `main.ts` 管。分享、Motion、Skill Discovery 先原样带上，不在升级片里删。

**工作量。** L。

### B. 卡通小人

**现在在哪。** 用户看到的那个，是 fork 自己加的首页英雄图，不是上游的桌面宠物。

- 组件：`apps/web/src/components/home-hero/IDorisDesignLogo.tsx`（`c49ba6a301`）。viewBox `0 0 120 120`，圆头、灯泡、画笔，和 logo 里的像素女孩不是一个角色。
- 样式：`apps/web/src/styles/home/home-hero.css` 约 3451 行起。显示尺寸 **72×72 CSS 像素**，容器最宽 `min(380px, 86vw)`，旁边是逐字跳出的「iDoris Design」。动画：身体起伏、眨眼、挥笔、灯泡、颜料点。`prefers-reduced-motion: reduce` 时全部停住。
- 挂载：`apps/web/src/components/HomeHero.tsx` 把原来的 WebGL `PixelScanLogo` 换成这个组件。

另一套是 Codex 宠物，默认不出现。`apps/web/src/state/config.ts` 的 `DEFAULT_PET.enabled` 和 `adopted` 都是 `false`。`App.tsx` 里 `clientType === 'desktop'` 时不挂 `PetOverlay`；Agent24 的 webview 没有 OD host bridge，`detectClientType()`（`apps/web/src/analytics/identity.ts`）会落到 `'web'`，所以一旦用户在设置里领养，宠物会浮在页面上。规格在 `apps/web/src/components/pet/codexAtlas.ts`：PNG 或 WebP，透明底，**1536×1872**，8×9 格，每格 **192×208**。行依次是 idle、running-right、running-left、waving、jumping、failed、waiting、running、review。叠层实际只画到大约 56–72px。

v0.26.1 的 Home 测试写明大 logo 已撤掉，标题改走 `RotatingTitleWord`。升级之后没有现成的 `IDorisDesignLogo` 槽，要在新 hero 上重新找一个位置（标题上方）。

**资产。** 英雄位用 SVG，72×72 显示，`shape-rendering: crispEdges`。旁边的字继续用 HTML，不画进 SVG。状态用同一张图上的分组：`idoris-state-idle`（默认）、`thinking`、`success`、`error`，由组件的 `data-state` 决定显示哪一组。现在的组件没有状态机，第一版可以只接 idle，分组先留在文件里。浮动宠物不在这一片里换；默认关着。若以后要换，再按 8×9 图集做，不要把 72px 的 SVG 拉去当宠物。

草稿：[`assets/idoris-mascot-draft.svg`](assets/idoris-mascot-draft.svg)。取色见文末规格。

**OD fork 改哪些。** 新 hero 上的挂载点、`IDorisDesignLogo.tsx`（或换掉这个文件）、`home-hero.css` 里 `.idoris-logo*`、`home-logo-assets.test.ts`。

**Agent24 改哪些。** 无。图在 OD 页面里。

**风险。** 新 Home 的标题是产品结构（旋转名词、dock 折叠）。小人只能加在标题旁，不要把 `RotatingTitleWord` 删掉，否则 v0.23 之后的 Home 行为会退回去。

**工作量。** S（用草稿顶上）到 M（按 logo 原图重描一版可发布的像素图）。

### C. 去掉右上角 GitHub 和充值按钮

**在哪。** 行号都是当前 fork HEAD（v0.22.2 + 补丁）。升到 v0.26.1 之后位置会挪，用 `data-testid` 找。文件是 `apps/web/src/components/EntryNavRail.tsx`。

- GitHub：`EntryTopRightCluster` 里 `data-testid="entry-top-right-github"` 的 `<a>`，约 866–882 行，链到 `REPO_URL = 'https://github.com/nexu-io/open-design'`，带星数。`onClick` 打 `trackAccountAction('github')`。
- 充值/积分：同一簇里 `data-testid="entry-top-right-credits"` 的按钮，约 892–906 行。点下去 `window.open` 到 workspace 的 billing console（注释写明余额、充值和自动充值已经搬到云端控制台，vela #1055）。

Home 走 `EntryNavRail` 末尾的 `EntryTopRightCluster`。项目页走 `App.tsx` 约 5526 行的 `WorkspaceTopRightAccountCluster`，内部还是同一个 cluster。两处一起消失。

删这两个控件不影响本地发消息。账户头像、升级按钮、余额弹窗还在，那是下一片的事。`WhatsNewPopup.tsx`、`UpdateDialog.tsx`、`DesignFilesPanel.tsx` 里另有 GitHub 链接，不是右上角这颗按钮，这一片不动。

**OD fork 改哪些。** `EntryNavRail.tsx` 及对应测试（搜 `entry-top-right-github`、`entry-top-right-credits`）。

**Agent24 改哪些。** 无。

**风险。** 若这一片先于账户切割发布，积分用尽时仍会弹出 AMR 余额窗，只是右上角没有入口。可以接受，因为下一片会把那条云路径关掉。

**工作量。** S。

### D. 移除用户系统

OD 的「用户」和「本地 agent 的登录」不是一件事。

- **要切的**：Open Design 云账户。AMR / Vela / Hosted、积分、充值、套餐、团队 workspace、邀请、在线状态、云端分享和评论同步。上游在 `apps/daemon/src/routes/vela.ts`：`AMR_API_UPSTREAM_ORIGIN = 'https://amr-api.open-design.ai'`。
- **要留的**：`apps/daemon/src/runtimes/auth.ts` 是 Claude / Codex / 其他 CLI 自己的登录诊断，不是 OD 账号。BYOK API key 也留着。`apps/daemon/src/integrations/xai-oauth.ts` 是某个媒体供应商的 OAuth，不是 OD 账户；默认不动，拍板项里单列。
- **我们自己的运行时不依赖 OD 登录**：`agent24` agent 只 spawn `agent24 acp`。v0.21.1 的发布标题就是 *Community First: No Login Required*。v0.25 仍把 Local AI 和 API Key 留在登录之外（#8567）。

**规模。** 文件名里带 vela / amr / billing / oauth / collab / wallet / sign-in / credits 的 TypeScript，非测试 **148** 个，含测试 **342** 个。`WorkspaceCollabContext` 被 **262** 个文件引用。`fetchAmrWallet` 21 个文件，`CloudSignIn` 31 个，`oauth` 83 个。这不是一个目录能删干净的。

余额门只挡云 agent：`apps/web/src/runtime/amr-balance-gate.ts` 写明 hard block 的条件是未登录或钱包 ≤ $0，对象是 OpenDesign Cloud agent 的发送。本地 agent 不走这道门。

**最小切割。**

1. 新增接口，默认本地单用户，不发云请求。
   - Web：`apps/web/src/identity/IdentityBridge.ts`。`LocalSingleUser` 返回 `{ id: 'local', displayName: 'iDoris', cloud: null }`。以后 Agent24 用和主题一样的通道把真实用户填进来。
   - Daemon：`apps/daemon/src/identity/cloud-account.ts`。`DisabledCloudAccount` 是默认实现。`registerVelaRoutes`、`routes/collab-*.ts` 在这个实现下不注册，或注册成固定 404。不要在每个调用点撒 `if`。
2. UI 不再挂登录和账户：`AmrLoginPill.tsx`、`CloudSignInTip.tsx`、`SignOutConfirmDialog.tsx`、`AmrBalanceDialog.tsx`、`AmrArtifactUpgradeDialog.tsx` / `Gate` / `HomeCard`，以及 `EntryNavRail` 里剩下的头像和升级。设置里的账户页改成一段「当前是本机用户，云账户未接入」。
3. 默认 agent 不要是 AMR。`apps/daemon/src/runtimes/defs/amr.ts` 留在树里，但从默认选择和 onboarding 的第一项拿掉。`amr-balance-gate` 在 `DisabledCloudAccount` 下直接 `allow`。
4. 协作和分享代码留在仓库里，入口关掉。v0.26 的公开链接和评论同步会因此不可用。这是预期，直到 iDoris 自己的身份接上 `IdentityBridge`。

物理删除那 148 个文件要等云端口稳定关掉、测试不再引用之后再做，单独立项。现在删，会在 262 处引用上炸开，而且下一轮 rebase 会把上游继续长出来的 collab 再合回来。

**切掉之后还要登录才能用的功能。**

| 功能 | 还要 OD 云账户吗 |
|---|---|
| 本地项目、原型、Agent24 ACP、BYOK、本机 CLI | 不要 |
| Hosted / AMR 模型、积分、Coding Plan | 要，关掉 |
| 团队 workspace、邀请、presence、跨设备覆盖同步（v0.25 #8683） | 要，关掉 |
| 公开分享链接、评论同步、把评论交回 Agent（v0.26 #8408） | 要，关掉 |
| 插件市场（GitHub 源） | 不要 OD 账户 |
| 媒体供应商自己的 key / xAI OAuth | 不是 OD 账户 |

**OD fork 改哪些。** 上表的新接口文件；`apps/daemon/src/server.ts` 的路由注册；`EntryNavRail.tsx`、`App.tsx` 里 cluster 和 AMR gate 的挂载；onboarding 里把 Hosted 放第一位的那一段（v0.25 #8567 落点，升级后按文件名再定位）。

**Agent24 改哪些。** 这一片可以不动。预留：将来外壳登录成功后，向 creative view `postMessage` `{ type: 'agent24:identity', user }`。不要在这一片就接 iDoris 账号。

**风险。** 只藏按钮、不关 `registerVelaRoutes`，页面仍会在后台打 `amr-api.open-design.ai`。验收必须看网络，不能只看 UI。workspace 上下文若返回空，项目列表有一批代码假定它存在（`App.tsx` 里 `useWorkspaceContext`），桩要返回「个人、本地、无账单」的空上下文，而不是 `null` 让页面崩。

**工作量。** L。物理删除另计，更大，本计划不做。

### E. 深浅色主题

**OD 现在怎么做。** `apps/web/src/state/appearance.ts`：`FORCED_APP_THEME = 'light'`。注释写明产品去掉了主题设置，因为「workspace surfaces have no dark tokens」，所以把 `data-theme` 永远写成 `light`，避免系统深色从「属性缺失」这条路径漏进来。`resolveAppTheme` 把任何存过的 `dark` / `system` 都折回 `light`。`applyAppearanceToDocument` 还调用 `getOpenDesignHost()?.appearance?.setTheme('light')`。v0.26.1 的同名文件仍是这段逻辑（`git grep FORCED_APP_THEME open-design-v0.26.1`）。

深色变量其实在。`apps/web/src/styles/tokens.css` 约 184 行：`[data-theme="dark"]` 一套，以及 `@media (prefers-color-scheme: dark)` 里 `html:not([data-theme])` 一套。强制写上 `data-theme="light"` 之后，第二套不会生效。部分组件用硬编码色，不吃变量，所以上游才认为深色是坏的。

**Agent24 现在怎么做。** `apps/desktop/src/renderer/App.tsx`：`darkMode` 默认 `true`，根节点 class 是 `app` 或 `app light`。主进程没有 `nativeTheme`。Creative 是另一个 `WebContentsView`，外壳的 class 传不进去。webview 的 `prefers-color-scheme` 跟操作系统走，但 OD 又把属性钉成 light，所以 Agent24 深色、Design 浅色。

**方案。** 不要只改 Electron 的 `nativeTheme`。钉死的属性比系统偏好优先。

1. OD：`applyAppearanceToDocument` 若收到宿主主题就用它，不再无条件写 light。没有宿主时保持今天的强制浅色，避免单独打开 OD 时行为大变。读取顺序：URL `?theme=dark|light`（仅首屏；现有 `canonicalCreativeOrigin` 禁止 origin 带 query，所以 query 要加在 `loadURL` 的路径上，例如 `origin + '/?theme=dark'`，并放宽 `main.ts` 里「当前 URL 是否同源」的判断，不要把 query 当成另一次导航），之后听 `message` 事件 `agent24:theme`。
2. Agent24：`showCreativeView` 首次加载带上当前 `darkMode`。渲染进程切换主题时 IPC 到主进程，主进程对 `creativeView.webContents.executeJavaScript` 派发同一事件。view 还没创建时只记住，下次加载用。
3. 视觉债单列：Home、对话、设置各看一遍深色。token 覆盖到的会跟着变；写死 `#fff` 的区块记下来，不挡这一片合并。

**OD fork 改哪些。** `apps/web/src/state/appearance.ts`，以及 `app/layout.tsx` 里若有首屏内联脚本也写了主题，要一起改（`appearance.ts` 注释提到 layout 的 pre-hydration accent 脚本）。加一个小测试：传入 `dark` 时属性是 `dark`，不传时仍是 `light`。

**Agent24 改哪些。** `apps/desktop/src/main/main.ts` 的 `showCreativeView`；渲染进程主题按钮所在的 `App.tsx` 加一条 IPC。preload 若还没有「通知 creative」的通道，就在现有 ipc 类型上加一条，不要让 webview 直接碰主进程内部。

**风险。** 深色不完整，第一眼会有浅色卡片剩在深色底上。这比现在整页浅色、外壳深色要好，但要在验收里写明已知缺口。`canonicalCreativeOrigin` 若被放得太宽，query 以外的东西会钻进来；只允许 `theme` 和下一片的 `locale` 两个参数。

**工作量。** M。把每个写死颜色的表面补齐是额外的 L，不放进这一片。

### F. 三语和升级流程

**OD 的 i18n。** 自研 React context，不是 i18next。`apps/web/src/i18n/types.ts` 的 `Locale` 有 19 个：`en`、`id`、`de`、`zh-CN`、`zh-TW`、`pt-BR`、`es-ES`、`ru`、`fa`、`ar`、`ja`、`ko`、`pl`、`hu`、`fr`、`uk`、`tr`、`th`、`it`。泰语文件是 `apps/web/src/i18n/locales/th.ts`（约 5377 行）。文案两层：`locales/*.ts` 是 UI 字典（`en.ts` 约 5392 行），`content.*.ts` 是长文案。键的约束来自 `Dict` 类型，漏键会在 typecheck 失败。`pnpm i18n:check`（`scripts/i18n-check.ts`）核对 locale 是否都登记进 `LOCALES` / `DICTS`，以及 README 语言切换器，**不**比较每个键的译文是否齐全。`pnpm i18n:coverage` 是另一份覆盖率报告。

语言选择在 `apps/web/src/i18n/index.tsx`：`localStorage` 键 `open-design:locale`，只有用户手动选过（`open-design:locale-source = manual`）才压过宿主。否则读 `getOpenDesignHost().client.osLocale`，再否则 `navigator.languages`，最后 `en`。Agent24 的 webview 没有这个 host，Chromium 又常报 `en-US`，所以外壳是中文或泰语时 OD 仍可能是英文。

Agent24 自己的三语在 `apps/desktop/src/renderer/App.i18n.ts`：`en`、`zh`、`th`。没有 `zh-TW`。

**方案。**

1. 外壳把语言传进来。映射：`zh` → `zh-CN`，`en` → `en`，`th` → `th`。和主题共用通道：首屏 `?locale=`，之后 `postMessage` / `executeJavaScript` 调用一个挂在 `window` 上的 `agent24SetLocale`（内部走现有 `setLocale`）。这次传入算宿主指定，不要写成 `manual`，否则用户改了系统语言后 OD 会被旧值钉死。若用户在 OD 设置里自己选了语言，再写入 `manual`，之后外壳不再覆盖。
2. 泰语不用新起一套。OD 的 `th.ts` 就是上游译文。fork 新增或改掉的句子放进覆盖层，例如 `apps/web/src/i18n/overlays/idoris/{en,zh-CN,th}.ts`，类型用 `Partial<Dict>`，运行时盖在上游字典上面。缺的键回退到上游，所以上游那 5000 多个键不用复制。
3. 升级流程放在 fork 仓库，不放在 Agent24：
   - 集成分支只接受「功能补丁」；品牌和三语不直接改 `locales/*.ts`。这样下次 rebase 不会再在 18 个字典上产生「两边都改第一行」的噪声。
   - 一条脚本（建议 `scripts/rebase-onto-upstream.sh`，只读检查 + 明确的 `git rebase` 子命令，默认不跑）列出：上游 tag、我们的补丁队列、`git merge-tree` 冲突路径。人确认后再 rebase。
   - `scripts/i18n-overlay-check.ts`：覆盖层的 `en` 有的键，`zh-CN` 和 `th` 必须有。接进 fork 的 `pnpm i18n:check`。上游字典的完整性继续靠 `Dict` 类型。
   - 每个上游版本一个集成分支 `integration/agent24-open-design-vX.Y.Z`，Agent24 的 pin 只指向该分支的 SHA。和现在钉 `34bc2ba810` 的方式相同。

**OD fork 改哪些。** `i18n/index.tsx` 的初始语言、overlay 目录、检查脚本、`package.json` 的 `i18n:check`。

**Agent24 改哪些。** 主题那条 IPC 扩成 `{ theme, locale }`。`App.i18n.ts` 不用搬进 OD。

**风险。** 把外壳语言写成 `manual`，用户会再也跟不上 Agent24 的切换。覆盖层若去改上游已有键的译文，rebase 后上游改了同一句，我们的覆盖仍赢，可能盖住安全相关的错误文案。覆盖层只放 fork 自己的产品名和空态，不改错误文案。

**工作量。** M。

### G. 命名和许可证

**许可证。** 根目录 `LICENSE` 是 Apache License 2.0。附录版权行是 `Copyright 2026 Open Design contributors`。`package.json` 的 `"license": "Apache-2.0"`。

和署名、商标有关的两条：

- 第 4 条：再分发时保留版权、许可、NOTICE。我们的修改可以另加版权声明和附加条款，但不能拿掉原文的许可。仓库根目前没有 `NOTICE`。
- 第 6 条：许可不授予许可方的商号、商标、服务标志或产品名，除非是为了说明作品来源，或为了照抄 NOTICE。

因此可以改界面名称，应当在关于或页脚说明来源，不应当让界面看起来像官方 Open Design 客户端，也不应当去掉 Apache 版权声明。

**建议。** 用户看到的名字用 **iDoris Design**。副标题用 **基于 Open Design**（英文 *Based on Open Design*），链到 `https://github.com/nexu-io/open-design`，放在设置或关于里，不放回右上角星标按钮。内部包名、目录、`od` 命令行保持 `open-design`，减少 rebase 时的重命名冲突。fork 的修改在 `LICENSE` 旁加一段 iDoris 版权，不替换 2026 Open Design contributors 那一行。

现在的半截品牌要收口：英文本 `app.brand` 已是 `iDoris Design`，简中仍是 `OpenDesign`，副标题两边都还是 Nexu Labs。改名片把副标题换成「基于 Open Design」，三种语言一起改，并放进 F 的覆盖层，而不是再去改 19 个上游文件。

不建议用「iDoris Version / OpenDesign」当主名：读起来像版本号，也把上游商标放在了主位置。

**OD fork 改哪些。** overlay 里的 `app.brand` / `app.brandSubtitle`；关于页若有独立组件再改那一处；`LICENSE` 附加版权段。

**Agent24 改哪些。** 导航目前是 `nav.creative: 'Design'`（`App.i18n.ts` 三种语言都是英文 Design）。若外壳也要改成「iDoris Design」，只改这三行。建议外壳保持「Design」，页面内再用全名，避免侧栏过长。这是拍板项。

**风险。** 低。商标风险来自把成品宣传成 Open Design 官方产品，不来自保留「基于 Open Design」这句。

**工作量。** S。

## 分片执行计划

顺序不能倒。小人挂在即将被上游删掉的 hero 上；账户切割要在 v0.26 的新分享代码上进行，否则会切完再被升级冲掉。每一片单独出包：fork 合进自己的集成分支并打一个可打包的 SHA，Agent24 只在需要改 pin 或外壳时另开 PR。ODX-1 之后 pin 更新一次；ODX-2 到 ODX-7 若只改 OD 源码，Agent24 要发新组件包（SHA 变了），桌面 pin 跟着动，此外无行为依赖。

### ODX-1 — 升到 v0.26.1，集成照旧

依赖：无。

内容：新集成分支基于 `open-design-v0.26.1`。带回 agent24 runtime、headless 启动器、`resourceSafeBase`。Home 和 `app.brand` 用上游。手合 `sidecars.ts`、`win/app.ts`。Agent24 更新 pin 和 SHA。

验收：

- Design 页能打开，origin 仍是 `127.0.0.1`。
- 一次本地生成走 `agent24` runtime（`agent acp`），产物仍能落在对话里。
- `resourceSafeBase` 单测仍拒绝信任根之外的路径。
- macOS 校验里 opencode 仍是 `open-design/bin/libexec/opencode/opencode`。
- 首页可以暂时还是上游的旋转标题，不要求出现 iDoris 小人。

### ODX-2 — 去掉右上角两颗按钮

依赖：ODX-1。

验收：Home 和项目页都没有 `entry-top-right-github`、`entry-top-right-credits`。本地发送仍可用。更新弹窗、设计文件面板里的 GitHub 链接还在。

### ODX-3 — 云账户改本地单用户桩

依赖：ODX-1。可与 ODX-2 并行开发，合入在 ODX-2 之后，避免两片改同一段 cluster。

验收：

- 新数据目录打开 Design，没有 OD 登录、没有积分、没有升级。
- 抓包或 mock：进程不请求 `amr-api.open-design.ai`。
- 用 Agent24 runtime 或 BYOK 能创建项目并跑完一轮。
- 分享入口不可用，且有一句人话说明，而不是红色未捕获错误。
- 仓库里能指出 `IdentityBridge` 和 `DisabledCloudAccount` 两个文件。

### ODX-4 — 主题跟着 Agent24

依赖：ODX-1。与 ODX-3 无代码依赖，可并行，验收时用已经去掉云按钮的包更好，但不是必须。

验收：Agent24 深色启动后，OD 的 `<html data-theme>` 为 `dark`；点外壳切换，OD 在同一次会话里变成 `light`，再点回来是 `dark`。不重启 view。单独把 OD 当网页打开、不带参数时仍是浅色。

### ODX-5 — 语言跟着 Agent24，覆盖层就位

依赖：ODX-1。和 ODX-4 共用外壳通道，建议 ODX-4 先合，ODX-5 在同一条 IPC 上加 `locale`。

验收：外壳 `zh` / `en` / `th` 分别让 OD 使用 `zh-CN` / `en` / `th`（看 `document` 上的语言或一处已知文案）。在 OD 里手动改语言后，外壳再切换不会覆盖。`pnpm i18n:check` 在覆盖层缺泰语或简中键时失败。

### ODX-6 — 换成 iDoris 小人

依赖：ODX-1（新 Home 已经在）。不依赖账户和主题，但深色下要看一眼镜描是否还在。

验收：Home 标题附近是像素风 iDoris，不是 `c49ba6a301` 的圆头小画家。显示约 72px。系统「减少动态」打开时动画停止。`home-logo-assets` 测试指向新图。浮动宠物默认仍关闭。

### ODX-7 — 命名和署名

依赖：ODX-5（文案走覆盖层）。可与 ODX-6 同一包验收，但要能单独 revert。

验收：简中、英文、泰语的产品名都是 iDoris Design，副标题是「基于 Open Design」而不是「由 Nexu Labs 出品」。关于或设置里有上游仓库链接。`LICENSE` 仍是 Apache-2.0，Open Design contributors 版权还在，并有 iDoris 修改的附加版权。右上角星标按钮没有回来。

## 需要 jason 拍板的问题

1. **小人谁来画。** 推荐：ODX-6 先用本文的 SVG 草稿顶上，发布前用 `logo-welcome.png` 重描一版同构图的像素图替换草稿。不做 8×9 宠物图集，除非还要把默认关闭的浮动宠物也换成她。
2. **分享和云功能留不留。** 推荐：这一轮关掉入口（`DisabledCloudAccount`），源码留着，等 iDoris 身份接上 `IdentityBridge` 再决定删不删。不要在升级里保留「登录 OD 才能分享」。
3. **对外名字。** 推荐：页面内「iDoris Design · 基于 Open Design」；Agent24 侧栏维持「Design」；包名继续 `open-design`。
4. **深色不完整也要跟外壳吗。** 推荐：要。v0.26.1 仍强制浅色，等上游不会解决这个错位。第一片只保证 token 表面跟着走，写死的颜色后面补。
5. **CLI 登录和 xAI OAuth 算不算「用户系统」。** 推荐：不算。`runtimes/auth.ts` 和 BYOK 留下。xAI OAuth 留下但不要出现在默认首页。
6. **简中副标题现在还写着 Nexu Labs，英文本品牌已经是 iDoris Design。** 推荐：ODX-7 三语一起改完，不在 ODX-1 里零星改字典。

## 小人设计规格

参考图：

- `apps/desktop/src/renderer/assets/logo-welcome.png`，192×192，本次取色用的就是它。
- `apps/desktop/assets/logo-original.png`，477×523。任务里写的 `renderer/assets/logo-original.png` 不存在，原图在 `apps/desktop/assets/`。

从 `logo-welcome.png` 不透明像素取样（Pillow，按 8 或 16 量化后的众数）：

| 角色 | hex | 怎么来的 |
|---|---|---|
| 头发、羽绒服 | `#151515` | 暗部平均；众数还有 `#202020`、`#181818` |
| 羽绒服暗褶 | `#202020` | 暗部第二众数 |
| 兜帽、胸前字、鞋头高光 | `#FAFAFA` | 近白像素平均，众数 `#F8F8F8` |
| 皮肤 | `#F8E8D0` | 肤色带众数；整段平均约 `#E5C3A7` |
| 橙色点、AI 徽章、袖口 | `#F07010` | 橙像素量化后最多的一档；同带还有 `#F08010`、`#F06000` |
| 眼睛 | `#202020` | 与头发同系，靠位置区分 |
| 腮红 | `#E8A090` | 草稿里少量使用，原图像素里不是大面积色 |

英雄槽（替换 `IDorisDesignLogo`）：

- 格式：SVG，整数坐标，`shape-rendering: crispEdges`。不要用 120 viewBox 的圆头矢量。
- 显示：宽 72px，高按 viewBox 32×40 约为 90px。旁边 HTML 字号维持现在的 `clamp(26px, 6vw, 38px)`。
- 构图：正面，黑马尾在画面右侧，黑羽绒服，胸口白色字牌（72px 下放不下完整「iDORIS」，完整词放在旁边的 HTML；胸口只留字牌）。画面右侧袖子一块 `#F07010`，上面一个白点当 AI 徽章。四周少量橙点，对应原图漂着的橙像素。
- 状态和现有动画的对应：现在没有状态，只有循环动画（`idoris-bob`、`idoris-blink`、`idoris-paint`、`idoris-splat`）。新图用分组代替画笔循环。

| 状态 | SVG 组 | 什么时候用 | 对应旧动画 |
|---|---|---|---|
| 待机 | `#idoris-state-idle` | 默认 | bob + blink |
| 思考 | `#idoris-state-thinking` | 有运行在进行 | 原来的挥笔 / 灯泡 |
| 成功 | `#idoris-state-success` | 这一轮产物写完 | 原来的颜料点，改成短促一次 |
| 出错 | `#idoris-state-error` | 运行失败 | 旧图没有，新加 |

同一时刻只显示一个状态组。`prefers-reduced-motion` 时不播放位移，状态组仍可切换。

宠物槽（本计划不改）：`PetOverlay` + `codexAtlas.ts` 的 8×9。若以后要她也出现在那里，idle / waiting 对思考，failed 对出错，review 对成功，running-* 可以空着或用待机。不要把这份 32×40 的 SVG 直接塞进图集。

草稿文件只表达几何和配色，胸前的字牌在 32 格里是示意，不是最终字模。
