# OSI-1 领域 OS 按登记来源安装

2026-11-17。只做 Sin90 这一条主路径。

## 登记表

`apps/desktop/src/shared/os-package-registry.ts` 的 `OS_PACKAGE_REGISTRY` 只登记 `sin90`：

- 仓库 `iDoris-ai/Sin90`
- 最新版只认 `https://api.github.com/repos/iDoris-ai/Sin90/releases/latest` 里的 tag `vX.Y.Z`
- 资产名 `sin90-{version}-macos-{arch}.tar.gz`（`arm64` / `x64`）
- 校验文件 `SHA256SUMS`，与资产同一 tag
- 下载 URL 必须是 `https://github.com/iDoris-ai/Sin90/releases/download/<tag>/<文件名>`，不能带查询串或别的主机

`cos72` 不在表里。页面只显示未安装，没有安装按钮。IPC 对未登记的 id 直接 400，不会发请求。

## 信任链

1. 登记表决定去哪个 URL。它不是信任根。
2. `SHA256SUMS` 只用来发现下载损坏或被换包。对不上就拒绝，不调用 `os install`。
3. 解压拒绝绝对路径、`..` 和链接条目，避免 tar 写到临时目录外面。gzip 展开不超过 1 GiB，条目不超过 10000，条目内容累计不超过 1 GiB。发布 JSON 和 `SHA256SUMS` 的响应体不超过 64 KiB。
4. 安装命令是应用自带的 `agent24 os install <解压目录>`。签名和 `files.json` 仍由这条命令离线校验（Developer ID，Team HYAP444FN9），与 documents 模块同一条路。桌面不再写一套验签。
5. 解压之后、卸载之前读解出目录的 `domain-os.yml`。`name` 必须等于登记 id，`version` 必须等于该发布的版本（tag 去掉开头的一个 `v`）。对不上就拒绝，已装目录不动。
6. 已装同版本返回 409 `already_installed`。本地版本比发布更新时也不降级。已装更旧才升级，顺序是：`agent24 package-integrity verify --root <解出目录>`（只读校验 files.json 和原生格式，不验签名；失败则不卸载），把 `<packagesRoot>/<id>` 整目录复制到本次工作目录，再 `os uninstall <id>`，再 `os install`。`os uninstall` 会撤销该模块的 consent grants。新包装不上时用备份再执行 `os install <备份目录>`：成功返回 `upgrade_failed_rolled_back`，备份也失败返回 `upgrade_failed_rollback_failed`。回滚只装回包目录，不恢复已撤销的授权。这两条命令都不改数据目录，也不改 `os.json`。

## 页面

Sin90 页读 `GET /api/v1/os`：`mounted` 显示已安装的版本；不在列表里显示未安装，并给出「安装最新版」。组件行 `failed` 时用已有的 `packageIntegrityHint`。能力中心对 sin90 / cos72 用同一条组件状态，不再落回「Built-in / 内置」。默认钉选本来就没有 sin90；已经钉过的偏好不改。

## 服务模式

- 本应用拉起的子 daemon：与 documents 一样，`whileStopped` 里停、装、再起。
- 已连上的 LaunchAgent：先走现有的 elevated `POST /api/v1/shutdown`（本机确认框），等进程退出，再执行 uninstall/install，然后 `launchctl kickstart -k`。外面套着 `whileServiceRestarts`，避免桌面在空窗期自己再起一个 daemon，装完后按原有探测重新连上服务 daemon。用户拒绝停服务则不安装。
- 没连上的外部 daemon，或开了常驻服务但还没 enrol：不杀进程。若 CLI 报锁被占用，界面说明先 `agent24 daemon stop`，或在设置里关掉常驻服务后再试。

## FU

- Linux / Windows 资产、以及非 `vX.Y.Z` 的预发布。
- 下载中断后续传、GitHub 限流、超过 512MB 的包。
- pax 长文件名以外的 tar 扩展、解压时磁盘写满。
- 有人改数据目录或 `os.json`、包目录是符号链接、并发交错安装。
- 服务 daemon 退出码不是 0、kickstart 成功但 enrol 被拒。
- 能力中心和 Sin90 页分别读 components 与 `/api/v1/os`，两者短暂不一致时不额外对齐。
