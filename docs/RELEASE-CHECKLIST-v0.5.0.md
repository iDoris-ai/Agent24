# Release checklist — v0.5.0（ME4-6.0.1，专用清单，冻结版）

> 本清单只管 v0.5.0 这一次发布，逐项精确到文件名，供 ME4-6.1.2（发布 Agent24）、
> ME4-6.0.2（Sin90/Cos72 各自发布）与 ME4-6.1.3（Mac mini 干净机器验收）直接照抄执行。
> 通用发布流程（安装器、"mother test"）见 `docs/RELEASE-CHECKLIST.md`（v0.1.0 版，仍适用于
> 桌面 dmg；v0.5.0 不重复上传 dmg，见下）。构建与打包方式照 v0.4.0 实际产出
> （`git show bb1945d`、`gh release view v0.4.0`）保持同一结构，不引入新形状。

## 0. 发布物清单（三仓库，精确到文件名）

### Agent24（`iDoris-ai/Agent24`，本仓库）

- **tag**：`v0.5.0`
- **发布物**：
  - `agent24-0.5.0-macos-arm64.tar.gz`
  - `SHA256SUMS`
- **tar.gz 内容**（与 v0.4.0 同构，顶层一个目录）：
  ```
  agent24-0.5.0-macos-arm64/
  agent24-0.5.0-macos-arm64/agent24
  agent24-0.5.0-macos-arm64/agent24d
  ```
- **`agent24-os-sdk` / `agent24-os-fd`**：仍是 `0.1.0`，版本线独立于 Agent24 主版本
  （见 ADR / ME4-S3-os-sdk.md §7/§8 Q9），**本次不发**新 tag、不产出新发布物。

### Sin90（`iDoris-ai/Sin90`，独立仓库）

- **tag**：`v0.5.0`（Sin90 自己 `Cargo.toml` 的 package version 本身就是 `0.5.0`，tag 与
  crate 版本对齐，不是套用 Agent24 的版本号）
- **发布物**：
  - `sin90-0.5.0-macos-arm64.tar.gz`
  - `SHA256SUMS`
- **tar.gz 内容**（解包后顶层一个目录，目录内**恰好**两项，不多不少）：
  ```
  sin90-0.5.0-macos-arm64/
  sin90-0.5.0-macos-arm64/domain-os.yml
  sin90-0.5.0-macos-arm64/bin/sin90
  ```
  `bin/sin90` 是 `cargo build --release` 产出的二进制（macOS arm64）。
- **打包脚本**：`scripts/package.sh`（Sin90 仓库根目录），产物输出到 `dist/`。
  脚本职责：`cargo build --release` → 建临时目录
  `sin90-0.5.0-macos-arm64/{domain-os.yml,bin/sin90}` → `tar -czf` → `shasum -a 256` 写
  `dist/SHA256SUMS` → 校验产物与清单逐项相等。

### Cos72（`MushroomDAO/Cos72`，独立仓库）

- **tag**：`v0.1.0`（Cos72 首次对外发布，不是 `0.5.0` —— 它是本轮才落地的独立仓库，
  与 Agent24/Sin90 的版本线无关）
- **发布物**：
  - `cos72-0.1.0-macos-arm64.tar.gz`
  - `SHA256SUMS`
- **tar.gz 内容**（结构同 Sin90，只换名字与版本号）：
  ```
  cos72-0.1.0-macos-arm64/
  cos72-0.1.0-macos-arm64/domain-os.yml
  cos72-0.1.0-macos-arm64/bin/cos72
  ```
- **打包脚本**：`scripts/package.sh`（Cos72 仓库根目录），产物输出到 `dist/`，职责同 Sin90
  一节。

## 1. 发布命令前断言（三仓库通用，逐条执行，任一失败即停）

对每个仓库（Agent24 / Sin90 / Cos72）各自在其 checkout 里执行：

- [ ] **`HEAD == 已审批 SHA`**：`git rev-parse HEAD` 的输出必须等于该仓库发布 PR
      被批准（APPROVED）时评审所见的那个合并提交 SHA。不匹配就停下，不得对着一个
      评审没看过的 commit 打 tag。
- [ ] **工作区干净**：`git status --porcelain` 空输出。
- [ ] **对应 tag 尚不存在**：`git rev-parse -q --verify refs/tags/<tag>` 必须失败
      （即该 tag 还没打过）。Agent24 = `v0.5.0`；Sin90 = `v0.5.0`；Cos72 = `v0.1.0`。
- [ ] **`shasum -a 256 -c SHA256SUMS` 通过**：在 `dist/`（或产物所在目录）下执行，
      对 tar.gz 逐一校验，输出必须是 `OK`。
- [ ] **`tar -tzf <name>.tar.gz` 列出的内容与上面「0. 发布物清单」逐项相等**：
      不多一个文件、不少一个文件、路径逐字符相同（含顶层目录名）。

## 2. 构建命令（照 v0.4.0 实际做法，不引入新脚本）

### Agent24

v0.4.0 未使用打包脚本，本次沿用手工命令（与 v0.4.0 tar 结构核对过，逐字节同构）：

```bash
cd rust
cargo build --release --bin agent24 --bin agent24d
mkdir -p /tmp/agent24-0.5.0-macos-arm64/agent24-0.5.0-macos-arm64
cp target/release/agent24 target/release/agent24d \
   /tmp/agent24-0.5.0-macos-arm64/agent24-0.5.0-macos-arm64/
cd /tmp/agent24-0.5.0-macos-arm64
tar -czf agent24-0.5.0-macos-arm64.tar.gz agent24-0.5.0-macos-arm64
shasum -a 256 agent24-0.5.0-macos-arm64.tar.gz > SHA256SUMS
```

（v0.4.0 的 `SHA256SUMS` 只含一行——整份 tar.gz 的校验值，不逐个二进制单独列一行；
v0.5.0 保持同一格式。）

### Sin90 / Cos72

各自仓库的 `scripts/package.sh`：

```bash
bash scripts/package.sh
shasum -a 256 -c dist/SHA256SUMS   # 就地自检
```

## 3. 干净机器验收（ME4-6.1.3，Mac mini）

在 `ssh jason@100.107.243.106`（tailscale）上，**只用已发布的 GitHub Release 资产**，
不从源码构建、不用本地 checkout：

1. **安装 daemon**：下载 `agent24-0.5.0-macos-arm64.tar.gz` + `SHA256SUMS`，
   `shasum -a 256 -c SHA256SUMS` 通过后解压，把 `agent24`/`agent24d` 放上 PATH
   （或直接从解压目录调用）。
2. **下载、校验、解压 Sin90 与 Cos72 的包**：
   `sin90-0.5.0-macos-arm64.tar.gz` + 其 `SHA256SUMS`；
   `cos72-0.1.0-macos-arm64.tar.gz` + 其 `SHA256SUMS`。两者各自校验通过后解压。
3. **`agent24 os install <解压目录>`**：装两次，各指向解压出的顶层目录
   （`sin90-0.5.0-macos-arm64/`、`cos72-0.1.0-macos-arm64/`）。`install` 不经过
   daemon（见 `rust/apps/agent24-cli/src/main.rs` 约 552 行附近 `os_local`/`OsAction::Install`
   的实现与注释：写文件到 packages root，daemon 下次启动时才读取），所以顺序是
   下载 → 校验 → 解压 → `os install <目录>` → `agent24 daemon stop && agent24 daemon start`
   （或首次 `daemon start`）让新包生效。
4. **验收**：`agent24 os list` 中 `sin90` 与 `cos72` 都显示 `mounted`。

每条命令的真实输出记录进本次发布 PR / Release notes（ME4-6.1.3 的验收要求）。

## 4. 回滚

发布后如需撤回：**删除 GitHub Release 与对应 tag**（人工操作，不自动化）：

```bash
gh release delete v0.5.0 -R iDoris-ai/Agent24 --yes
git push --delete origin v0.5.0        # Agent24 仓库

gh release delete v0.5.0 -R iDoris-ai/Sin90 --yes
git -C <Sin90 checkout> push --delete origin v0.5.0

gh release delete v0.1.0 -R MushroomDAO/Cos72 --yes
git -C <Cos72 checkout> push --delete origin v0.1.0
```

不做「发新 patch 覆盖」——回滚就是真的把发布物撤回，需要重发时按本清单重新走一遍。
