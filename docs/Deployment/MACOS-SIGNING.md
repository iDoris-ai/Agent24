# macOS 签名与公证（DEP-B1/B2/B3）

可复现流水线：桌面端 dmg 和 CLI 裸二进制都用 Developer ID 签名、Hardened Runtime、notarytool 公证。脚本全部在仓库内，不依赖口头步骤。

| 产物 | 脚本 | 公证对象 | staple |
|---|---|---|---|
| 桌面端 dmg | `apps/desktop/scripts/release-mac.sh` | dmg | 是（dmg） |
| `agent24` / `agent24d` 等裸二进制 | `scripts/sign-cli-mac.sh` | zip | 否（裸 Mach-O 不支持，见下） |
| 验证 | `scripts/verify-mac-signing.sh` | - | - |

## 前置条件（一次性）

1. **签名证书在 login keychain**：
   ```bash
   security find-identity -v -p codesigning
   # 应见：Developer ID Application: Huifeng Jiao (HYAP444FN9)
   ```
   证书私钥丢失就无法签名，换机器要导出 p12（含私钥）再导入。
2. **notarytool keychain profile**（凭据只存在 keychain，不进仓库、不进环境变量）：
   ```bash
   xcrun notarytool store-credentials apple-notary-HYAP444FN9 \
     --apple-id <Apple ID> --team-id HYAP444FN9 --password <app-specific-password>
   # 或用 App Store Connect API key：--key AuthKey_XXXX.p8 --key-id <ID> --issuer <UUID>
   xcrun notarytool history --keychain-profile apple-notary-HYAP444FN9   # 能列出历史即可用
   ```
3. 工具链：Xcode Command Line Tools（`codesign`、`xcrun notarytool`、`stapler`）、Rust、pnpm（`pnpm install` 已完成）。

> DEP-B1 的"导出 p12 / API key 存仓库 Secrets"只在要把签名搬进 GitHub Actions 时才需要。当前签名在本机 keychain 完成，CI 接入留给后续（见"未自动化的点"）。

## 一条命令发布桌面端

```bash
bash apps/desktop/scripts/release-mac.sh          # 签名 + 公证 + staple + 验证
SKIP_NOTARIZE=1 bash apps/desktop/scripts/release-mac.sh   # 只签名，本地快速试
```

环境变量：

| 变量 | 默认 | 作用 |
|---|---|---|
| `AGENT24_SIGN_IDENTITY` | `Developer ID Application: Huifeng Jiao (HYAP444FN9)` | 签名身份（脚本会去掉前缀后交给 electron-builder） |
| `AGENT24_NOTARY_PROFILE` | `apple-notary-HYAP444FN9` | notarytool keychain profile |
| `SKIP_NOTARIZE` | 空 | `1` = 不公证；验证时不断言 Notarized，且 `spctl` 仅展示不断言（未公证的 Developer ID 被判 `Unnotarized Developer ID` 是预期）。`codesign --verify`、Hardened Runtime、`lipo` 仍为硬断言 |
| `AGENT24_ARCH` | `arm64` | 目标架构（`x64` 需在 Intel 机或对应工具链上） |
| `CARGO_TARGET_DIR` | `rust/target` | Rust 产物目录，**必须是绝对路径**（相对路径会被脚本拒绝）；脚本把 `agent24d` 复制到 `rust/target/release/` 供打包引用 |
| `CARGO_BUILD_TARGET` | 必须未设置 | 设了它 cargo 产物在 `<target>/<triple>/release`，脚本只读 `<target>/release`，可能打包陈旧产物，故检测到即报错退出 |

步骤：前置检查 → `cargo build --release` → node-daemon + vite/tsc → `electron-builder --mac`（签名）→ `notarytool submit --wait` → `stapler staple/validate` → `verify-mac-signing.sh`。任一步失败即退出；公证被拒时自动打印 `notarytool log`。产物在 `apps/desktop/release/`（已被 `.gitignore`，不要提交），含 `SHA256SUMS` 与 `notarize-submit.json`（submission id）。

### 配置选择：为什么公证不用 electron-builder 内置 `notarize`

- electron-builder 25 内置公证只认 `APPLE_ID`+app 专用密码 或 App Store Connect API key 环境变量，**不支持 keychain profile**；用它就得把明文凭据放进环境变量/CI secrets。
- 公证放在脚本里调 `notarytool --keychain-profile`：凭据留在 keychain，与 10-16 手工流程完全一致，且**公证对象是 dmg 本身**（内含 app 的票据一并登记），再 `staple` 到 dmg，离线机器打开 dmg 也能通过。
- 因此 `package.json` 里 `mac.notarize=false`，签名由 electron-builder 完成（`hardenedRuntime`、`entitlements`、`entitlementsInherit`、`binaries`、`dmg.sign`）。
- `mac.binaries` 显式列出 `Contents/Resources/backend/agent24d` 和 `Contents/Resources/backend/agent24`（路径相对 `.app`）。桌面端的文档导入和文档操作都调用打进包里的 `agent24` CLI（DOC-1-43、DOC-1-45），所以它和 `agent24d` 一样要签名、启用 Hardened Runtime。

### Entitlements（`apps/desktop/build/entitlements.mac.plist`）

仅 `allow-jit` + `allow-unsigned-executable-memory`（Electron/V8 必需）。**没有** `disable-library-validation`：Electron Framework 和内置原生模块同 Team ID，用不上。`agent24d` 子进程沿用 `entitlementsInherit`，没有额外权限。

## CLI 裸二进制（DEP-B3）

```bash
CARGO_TARGET_DIR=... cargo build --release -p agent24d -p agent24-cli --manifest-path rust/Cargo.toml
bash scripts/sign-cli-mac.sh                      # 默认处理 release 下的 agent24、agent24d
bash scripts/sign-cli-mac.sh bin/sin90 bin/cos72  # 或显式传入路径
```

`-o OUT_DIR` 可为相对或绝对路径（脚本开头转绝对）。OUT_DIR 必须不存在、为空，或含上次本脚本写下的标记文件 `.agent24-cli-out`（复跑时只清理标记里记录的产物与 `stage/`，不会整目录删除）；非空且无标记会拒绝运行。输入二进制 basename 重复（如 `a/tool b/tool`）会拒绝运行。

`codesign --force --options runtime --timestamp` → 打 zip → `notarytool submit --wait` → 输出 `dist/mac-cli/`（二进制、zip、`notarize-submission.json`、`SHA256SUMS`）。

**裸二进制不能 staple**：票据只能附着在 `.app/.pkg/.dmg`。分发 zip 即可：用户（浏览器下载带 quarantine）首次运行时 Gatekeeper 在线查询票据；完全离线机器首次运行会被拦。要离线可用，请把 CLI 装进 `.pkg` 或 dmg 再公证 staple（未做）。`tar` 解压或 `curl` 下载不带 quarantine，不触发 Gatekeeper，行为与未签名一致。

## Open Design 按需组件（darwin-arm64）

Open Design 不进 dmg：`release-mac.sh` 在打包前构建它、签名其原生模块、打成 tarball，**只有清单**
（`open-design-component.json`）通过 `extraResources` 进包；tarball 在 `.open-design-component-dist/`，
要手动上传到同名 GitHub release 的资产里（应用运行时从 `releases/download/<tag>/<资产>` 下载）。

- **必须设 `A24_RELEASE_TAG`**（如 `v0.6.0-preview.20261028.10`）。清单 URL 里的版本 = tag 去掉 `v`，
  不会回退到 package.json 的版本（那个 tag 不存在，用户会 404）。去掉前导 `v` 之后，tag 还必须以
  `apps/desktop/package.json` 的 `version` 开头：当前是 `0.6.0`，所以只接受 `v0.6.0` 或 `v0.6.0-…`。
- Open Design 源码默认 `~/Dev/auraai/open-design-agent24`（`A24_OPEN_DESIGN_SOURCE_DIR` 可改）。
  `check-source` 要求工作区干净（`git status --porcelain` 为空）并且 HEAD 等于固定 sha。
  prepare 在 Resources 根目录写 `.agent24-od-source.json`（`{sha, version, tree}`）。`tree` 覆盖每个普通文件的内容与权限、每条软链接的目标，以及每个目录（含根目录）的路径与权限。设置了
  `A24_OPEN_DESIGN_RESOURCES_DIR` 复用已有树时，stamp 必须存在且 sha、version、tree 都与 pin 和当前树一致，否则拒绝打包。
  本次改动之前生成的旧 stamp 不带目录和内容摘要，复用前必须重新跑 prepare。
  本地 stamp 不防「树和 stamp 被同时重写」，它只用来发现意外改动，不是防篡改手段。
- 组件清单的下载仓库写死为 `iDoris-ai/Agent24`，不读环境变量 `A24_OD_REPOSITORY`。
- **为什么原生模块必须同 Team ID 签名**：组件的 daemon 跑在 app 自己的二进制里（`ELECTRON_RUN_AS_NODE`），
  app 开了 Hardened Runtime 且没有 `disable-library-validation`，所以下载来的 `.node`/`.dylib` 不同 Team ID 就加载不了。
  `open-design-mac-release.mjs sign` 对组件树里每个 Mach-O 用 Developer ID + `--timestamp` 签名并 `codesign --verify --strict`。
  executable 和正式签名的库带 `--options runtime`。`.dylib` / `.node` 不挂 entitlements。
  JIT 两项（`allow-jit`、`allow-unsigned-executable-memory`）只给白名单里的 executable，目前只有 `opencode`（bun/JSC）。
  `ffmpeg`、`esbuild`、`spawn-helper`、`vela` 只签 Hardened Runtime。应用自己的
  `build/entitlements.mac.plist` 不变，仍给 Electron/V8 用。
- **组件 tarball 要不要公证**：应用自己 `fetch` 下载，文件没有 quarantine 标记，Gatekeeper 不评估它，能否加载只看 Team ID。
  松散文件也无法 staple。脚本仍会把 zip 提交公证（几分钟）以登记 cdhash，万一用户用浏览器手动下载资产也能放行；`SKIP_NOTARIZE=1` 时跳过公证。
  无论是否公证，`verify-mac-open-design.sh` 都会把 tarball 解到临时目录，对每个 Mach-O 跑
  `codesign --verify --strict`，并核对 `TeamIdentifier` 是 `HYAP444FN9`。
- `verify-mac-open-design.sh` 还校验：清单在包内且字段正确（version 来自 pack 模块的 pin，不再写死），
  下载 URL 的 owner/repo 只能是 `iDoris-ai/Agent24`，包内没有 Open Design 目录/tarball，资产 sha256/size 与清单一致，
  DMG 不超过 `A24_DMG_MAX_MB`（默认 250）。
- 脚本结束时打印上传命令：`gh release upload "$A24_RELEASE_TAG" "<tarball 绝对路径>" -R iDoris-ai/Agent24`。
  **发布之后**再跑可达性检查（读清单里的 URL、sha256、size；`curl -fsIL --max-time 60` 确认 URL 可达，服务器给了
  `Content-Length` 时再核对 size）：

  ```bash
  bash apps/desktop/scripts/check-open-design-release-asset.sh <dmg 或 open-design-component.json>
  ```

- 发布前本地验证（不上传 GitHub）：把 tarball 用本机 http 服务起来，启动已签名 app 时设
  `A24_OPEN_DESIGN_COMPONENT_LOCAL_BASE_URL=http://127.0.0.1:<port>/` 或 `http://[::1]:<port>/`。
  只接受这两个字面地址的 `http`，不接受 `localhost`，也不把主机名放进全局下载白名单。
  豁免只对这一个 origin（协议 + 主机 + 端口）生效，跳到别的端口或 `https` 会被拒绝。
  覆盖只改资产所在目录，文件名仍用清单里的名字。sha256 仍按包内清单校验。
  覆盖生效时下载卡片会显示「使用本地测试源」。

## 验证

```bash
bash scripts/verify-mac-signing.sh --expect-notarized --arch arm64 apps/desktop/release/Agent24-*.dmg
bash scripts/verify-mac-signing.sh /Applications/Agent24.app
```

dmg 路径：`spctl -t open --context context:primary-signature` + `stapler validate` → 只读挂载 → `ditto` 复制 app 到临时目录 → 卸载 → 对副本 `codesign --verify --deep --strict`、`spctl -a -vv`、逐个检查主程序/`agent24d`/其他 Mach-O 已签且带 `runtime` flag、`lipo -archs` 主程序与 `agent24d` 一致且与 dmg 文件名（arm64/x64）一致。

## 干净机验收（B5）

1. 在**没装过 Agent24、没登录开发者账号**的 Mac 上，用 Safari/Chrome 从 Release 页下载 dmg（带 quarantine 标记，`xattr` 可见 `com.apple.quarantine`）。
2. `shasum -a 256 -c SHA256SUMS`。
3. 双击打开 dmg，拖到 Applications，首次打开：不应出现"无法验证开发者"或"已损坏"，仅有"从互联网下载"的常规确认。
4. 可选断网重复一次（验证 staple）。
5. 终端：`spctl -a -t open --context context:primary-signature -v <dmg>` 应为 `accepted` + `source=Notarized Developer ID`。

## 常见失败

| 现象 | 原因 / 处理 |
|---|---|
| `notarytool` 返回 Invalid，日志含 "The binary is not signed with a valid Developer ID certificate" | 用了 Apple Development/ad-hoc 证书；检查 `AGENT24_SIGN_IDENTITY` 与 keychain |
| 日志 "not signed with a hardened runtime" / "no secure timestamp" | 某个嵌套二进制漏签；`verify-mac-signing.sh` 会列出 `UNSIGNED`/`NO-RUNTIME`。新增的随包 Mach-O 加进 `mac.binaries` |
| `Team ID` 不匹配（Electron Framework 与主程序不同 Team） | 混用了不同证书重复签名；清空 `apps/desktop/release/` 重来，确认 `CSC_*` 环境变量未设置（脚本会 unset） |
| 签名后 app 启动即崩溃（`EXC_BAD_ACCESS`/`JIT`、`V8 ... code range`） | entitlements 缺 `allow-jit` / `allow-unsigned-executable-memory`，或 `entitlementsInherit` 未指向 plist |
| 加载原生模块报 `code signature ... not valid for use in process: different Team IDs` | 引入了其他 Team 签名的 dylib/.node；确需则在 plist 加 `com.apple.security.cs.disable-library-validation` 并在此记录原因 |
| `notarytool` 报 keychain profile 不存在 | 重新 `store-credentials`（见前置 2） |
| electron-builder 报 identity 前缀错误 | `mac.identity` 不能带 `Developer ID Application:` 前缀（脚本已处理） |
| 校验时 `spctl` 为 `rejected` / `source=Unnotarized Developer ID` | 没公证或公证后又改动了 dmg（改动即作废票据）；公证必须是最后一步，之后只能 staple |

## 未自动化的点

- GitHub Actions 里的签名/公证（需要 DEP-B1 的 p12 + API key 进 Secrets，用临时 keychain 导入）。
- Intel（x64）dmg：脚本支持 `AGENT24_ARCH=x64`，但需 Intel 机或可用的交叉工具链，尚未实测。
- 裸 CLI 的离线可用（需 pkg/dmg 封装）。
- 干净机的浏览器下载验收是人工步骤（B5）。
