#!/usr/bin/env bash
# macOS 桌面端一条命令发布流水线（DEP-B2）：
#   build:rust(release) -> node-daemon -> vite/tsc -> electron-builder(签名) -> notarytool(dmg) -> staple -> 验证
# 用法：bash apps/desktop/scripts/release-mac.sh        （在仓库任意目录执行均可）
# 环境变量：
#   AGENT24_SIGN_IDENTITY  签名身份，默认 "Developer ID Application: Huifeng Jiao (HYAP444FN9)"
#   AGENT24_NOTARY_PROFILE notarytool keychain profile，默认 apple-notary-HYAP444FN9
#   SKIP_NOTARIZE=1        只签名不公证（本地快速试；此时不做 spctl 的 Notarized 断言）
#   CARGO_TARGET_DIR       Rust 产物目录（默认 rust/target）；产物会被复制到 rust/target/release 供打包引用
#   AGENT24_ARCH           目标架构，默认 arm64（可选 x64，需在对应机器/交叉工具链上）
#   A24_RELEASE_TAG        【必填】本次要发布的 GitHub release tag（如 v0.6.0-preview.20261028.10）。
#                          Open Design 按需组件的下载 URL = releases/download/<tag>/<资产>，写进包内清单；
#                          不会回退到 package.json 的版本（那个 tag 不存在，用户会 404）。
#   A24_OPEN_DESIGN_SOURCE_DIR  Open Design 源码 checkout，默认 ~/Dev/auraai/open-design-agent24，必须在固定 sha
#   A24_OPEN_DESIGN_RESOURCES_DIR  可选：已用 prepare-open-design-linux-resources.mjs 备好的 Resources 目录，
#                          设了就跳过 tools-pack 与 prepare（省约 5 分钟，复跑时用）
#   A24_DMG_MAX_MB         DMG 体积上限，默认 250（防止 Open Design 被重新打进包）
# 产物：release/*.dmg（只含清单）与 .open-design-component-dist/open-design-*.tar.gz（要上传到同一 release 的资产）
set -euo pipefail

SIGN_IDENTITY="${AGENT24_SIGN_IDENTITY:-Developer ID Application: Huifeng Jiao (HYAP444FN9)}"
NOTARY_PROFILE="${AGENT24_NOTARY_PROFILE:-apple-notary-HYAP444FN9}"
ARCH="${AGENT24_ARCH:-arm64}"

DESKTOP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$DESKTOP_DIR/../.." && pwd)"
VERIFY="$REPO_ROOT/scripts/verify-mac-signing.sh"
OD_RELEASE="$DESKTOP_DIR/scripts/open-design-mac-release.mjs"
OD_SOURCE_DIR="${A24_OPEN_DESIGN_SOURCE_DIR:-$HOME/Dev/auraai/open-design-agent24}"
OD_WORK_DIR="$DESKTOP_DIR/.open-design-build"
OD_STAGE_DIR="$DESKTOP_DIR/.open-design-resources"
OD_MANIFEST_DIR="$DESKTOP_DIR/.open-design-component"        # 唯一被 extraResources 打进包的目录：只放清单
OD_TARBALL_DIR="$DESKTOP_DIR/.open-design-component-dist"    # tarball 放这里，绝不能在上面那个目录里（C1）
ENTITLEMENTS="$DESKTOP_DIR/build/entitlements.mac.plist"

step() { printf '\n==> %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "只能在 macOS 上运行"

step "0/6 前置检查"
security find-identity -v -p codesigning | grep -F "${SIGN_IDENTITY}" >/dev/null \
  || die "keychain 中找不到签名身份：${SIGN_IDENTITY}（security find-identity -v -p codesigning）"
if [[ "${SKIP_NOTARIZE:-0}" != "1" ]]; then
  xcrun notarytool history --keychain-profile "${NOTARY_PROFILE}" >/dev/null \
    || die "notarytool profile 不可用：${NOTARY_PROFILE}（见 docs/Deployment/MACOS-SIGNING.md）"
fi
# Open Design 按需组件：tag 必填（早失败，别等十几分钟构建完才发现）
[[ -n "${A24_RELEASE_TAG:-}" ]] || die "必须设置 A24_RELEASE_TAG（本次发布的 GitHub tag，如 v0.6.0-preview.20261028.10）。清单里的下载 URL 要指向真实存在的 release，不能默认用 package.json 的版本"
OD_APP_VERSION="$(node "$OD_RELEASE" version "$A24_RELEASE_TAG")" || exit 1
if [[ -z "${A24_OPEN_DESIGN_RESOURCES_DIR:-}" ]]; then
  node "$OD_RELEASE" check-source "$OD_SOURCE_DIR" >/dev/null || die "Open Design 源码不在固定 sha：$OD_SOURCE_DIR"
fi
echo "release tag: $A24_RELEASE_TAG  -> 组件清单版本 $OD_APP_VERSION"
# electron-builder 的 identity 不能带 "Developer ID Application:" 前缀
BUILDER_IDENTITY="${SIGN_IDENTITY#Developer ID Application: }"
# S3：cargo 产物路径按 <target>/release 读取，下面两种环境会导致读到陈旧产物或解析错目录
[[ -z "${CARGO_BUILD_TARGET:-}" ]] || die "请 unset CARGO_BUILD_TARGET：设了它产物在 <target>/<triple>/release，脚本会读到陈旧产物"
[[ -z "${CARGO_TARGET_DIR:-}" || "${CARGO_TARGET_DIR}" == /* ]] || die "CARGO_TARGET_DIR 必须是绝对路径（当前：${CARGO_TARGET_DIR}）"
# 防止环境里的 CSC_* 把签名悄悄切到别的证书 / 或让 electron-builder 去找 p12
unset CSC_LINK CSC_KEY_PASSWORD CSC_NAME

step "1/6 构建 Rust release（agent24d、agent24）"
( cd "$REPO_ROOT" && cargo build --release --locked -p agent24d -p agent24-cli --manifest-path rust/Cargo.toml )
CARGO_OUT="${CARGO_TARGET_DIR:-$REPO_ROOT/rust/target}/release"
# 两个都进包：extraResources 的 backend/agent24d 和 backend/agent24（文档导入调用 agent24）
for b in agent24d agent24; do
  [[ -x "$CARGO_OUT/$b" ]] || die "缺少 $CARGO_OUT/$b"
  if [[ "$CARGO_OUT" != "$REPO_ROOT/rust/target/release" ]]; then
    mkdir -p "$REPO_ROOT/rust/target/release"
    cp -f "$CARGO_OUT/$b" "$REPO_ROOT/rust/target/release/$b"
  fi
  # 每次都从未签名的干净二进制起步，避免 electron-builder 对已签名文件重复处理
  codesign --remove-signature "$REPO_ROOT/rust/target/release/$b" 2>/dev/null || true
done

step "2/6 构建 node-daemon 与桌面端前端/主进程"
cd "$DESKTOP_DIR"
pnpm --filter @agent24/node-daemon build
pnpm build

step "2b/6 Open Design 按需组件（darwin-${ARCH}：构建 → 签名原生模块 → 公证 → 打 tarball + 清单）"
[[ "${ARCH}" == "arm64" ]] || die "Open Design 组件目前只支持 arm64（AGENT24_ARCH=${ARCH}）"
# 先清掉陈旧产物：旧清单 / 旧 tarball 绝不能混进这次的包
rm -rf "$OD_MANIFEST_DIR" "$OD_TARBALL_DIR" "$OD_STAGE_DIR"
if [[ -n "${A24_OPEN_DESIGN_RESOURCES_DIR:-}" ]]; then
  OD_RESOURCES="$A24_OPEN_DESIGN_RESOURCES_DIR"
  echo "复用已备好的 Open Design Resources：$OD_RESOURCES"
else
  rm -rf "$OD_WORK_DIR"
  mkdir -p "$OD_WORK_DIR"
  ( cd "$OD_SOURCE_DIR" && pnpm --filter @open-design/tools-pack dev mac build --to app --portable \
      --namespace agent24-release --dir "$OD_WORK_DIR/pack" --json > "$OD_WORK_DIR/tools-pack.json" )
  OD_APP="$(find "$OD_WORK_DIR/pack" -type d -name 'Open Design.app' -path '*/builder/mac-arm64/*' -print -quit)"
  [[ -n "$OD_APP" ]] || die "tools-pack 没有产出 Open Design.app"
  OD_RESOURCES="$OD_APP/Contents/Resources"
  node "$DESKTOP_DIR/scripts/prepare-open-design-linux-resources.mjs" "$OD_SOURCE_DIR" "$OD_RESOURCES"
fi
A24_OPEN_DESIGN_RESOURCES_DIR="$OD_RESOURCES" pnpm stage:open-design
# 与 release-desktop.yml 的 prune 步骤一致（只删可证明运行时不加载的东西）
find "$OD_STAGE_DIR" -name '*.map' -delete
rm -rf "$OD_STAGE_DIR/app/node_modules/@open-design/web" "$OD_STAGE_DIR/app/node_modules/next" \
  "$OD_STAGE_DIR/app/node_modules/@next" "$OD_STAGE_DIR/app/node_modules/openai" \
  "$OD_STAGE_DIR/app/node_modules/react" "$OD_STAGE_DIR/app/node_modules/react-dom"
A24_OPEN_DESIGN_SOURCE_DIR="$OD_SOURCE_DIR" pnpm rebuild:open-design-native
# 组件的 daemon 跑在 app 自己的二进制里（ELECTRON_RUN_AS_NODE），app 开了 Hardened Runtime 且没有
# disable-library-validation，所以组件里每个 .node/.dylib 必须同 Team ID 签名，否则下载成功也加载不了
node "$OD_RELEASE" sign "$OD_STAGE_DIR" "$SIGN_IDENTITY" "$ENTITLEMENTS"
if [[ "${SKIP_NOTARIZE:-0}" != "1" ]]; then
  # 组件由 app 自己 fetch，没有 quarantine 标记，Gatekeeper 不会评估它，加载只看 Team ID。
  # 仍提交公证：zip 里的 Mach-O 会按 cdhash 登记公证票据，用户若改用浏览器手动下载资产（带 quarantine）也能放行。
  # 松散文件无法 staple，所以这只是登记，没有产物要回写。
  OD_NOTARY_ZIP="$OD_WORK_DIR/open-design-notarize.zip"
  mkdir -p "$OD_WORK_DIR"
  rm -f "$OD_NOTARY_ZIP"
  ditto -c -k --sequesterRsrc "$OD_STAGE_DIR" "$OD_NOTARY_ZIP"
  OD_SUBMIT_JSON="$OD_WORK_DIR/open-design-notarize-submit.json"
  xcrun notarytool submit "$OD_NOTARY_ZIP" --keychain-profile "${NOTARY_PROFILE}" --wait --output-format json | tee "$OD_SUBMIT_JSON"
  OD_STATUS="$(/usr/bin/plutil -extract status raw -o - "$OD_SUBMIT_JSON")"
  OD_SUB_ID="$(/usr/bin/plutil -extract id raw -o - "$OD_SUBMIT_JSON")"
  if [[ "$OD_STATUS" != "Accepted" ]]; then
    xcrun notarytool log "$OD_SUB_ID" --keychain-profile "${NOTARY_PROFILE}" || true
    die "Open Design 组件公证未通过：status=$OD_STATUS id=$OD_SUB_ID"
  fi
  echo "Open Design 组件公证 submission id: $OD_SUB_ID"
  rm -f "$OD_NOTARY_ZIP"
fi
A24_OD_STAGE_DIR="$OD_STAGE_DIR" A24_OD_PACK_OUT_DIR="$OD_MANIFEST_DIR" A24_OD_TARBALL_OUT_DIR="$OD_TARBALL_DIR" \
  A24_OD_APP_VERSION="$OD_APP_VERSION" A24_OD_PLATFORM=darwin A24_OD_ARCH="${ARCH}" pnpm pack:open-design
OD_TARBALL="$(find "$OD_TARBALL_DIR" -maxdepth 1 -name 'open-design-*.tar.gz' -print | head -n1)"
[[ -f "$OD_TARBALL" ]] || die "未生成 Open Design tarball"
echo "open-design tarball: $OD_TARBALL"

step "3/6 electron-builder 打包 + 签名（${ARCH}，Hardened Runtime）"
rm -rf "$DESKTOP_DIR/release"
pnpm exec electron-builder --mac "--${ARCH}" -c.mac.identity="$BUILDER_IDENTITY"
DMG="$(find "$DESKTOP_DIR/release" -maxdepth 1 -name '*.dmg' -print | head -n1)"
[[ -f "$DMG" ]] || die "未生成 dmg"
echo "dmg: $DMG"

step "3b/6 验证 Open Design 不在包内、只带清单（C1）"
bash "$DESKTOP_DIR/scripts/verify-mac-open-design.sh" "$(find "$DESKTOP_DIR/release" -maxdepth 2 -type d -name 'Agent24.app' -print -quit)" "$DMG" "$OD_TARBALL_DIR" "$OD_APP_VERSION"

if [[ "${SKIP_NOTARIZE:-0}" == "1" ]]; then
  step "4-5/6 已跳过公证与 staple（SKIP_NOTARIZE=1）"
else
  step "4/6 提交公证（notarytool --wait，约数分钟）"
  SUBMIT_JSON="$DESKTOP_DIR/release/notarize-submit.json"
  xcrun notarytool submit "$DMG" --keychain-profile "${NOTARY_PROFILE}" --wait --output-format json | tee "$SUBMIT_JSON"
  STATUS="$(/usr/bin/plutil -extract status raw -o - "$SUBMIT_JSON")"
  SUB_ID="$(/usr/bin/plutil -extract id raw -o - "$SUBMIT_JSON")"
  if [[ "$STATUS" != "Accepted" ]]; then
    xcrun notarytool log "$SUB_ID" --keychain-profile "${NOTARY_PROFILE}" || true
    die "公证未通过：status=$STATUS id=$SUB_ID"
  fi
  echo "notarization submission id: $SUB_ID"

  step "5/6 staple + validate"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
fi

step "6/6 验证"
EXPECT=()
[[ "${SKIP_NOTARIZE:-0}" == "1" ]] || EXPECT=(--expect-notarized)
# bash 3.2 + set -u：空数组直接展开会报 unbound variable，必须用 ${arr[@]+"${arr[@]}"}
bash "$VERIFY" ${EXPECT[@]+"${EXPECT[@]}"} --arch "${ARCH}" "$DMG"

( cd "$DESKTOP_DIR/release" && shasum -a 256 "$(basename "$DMG")" > SHA256SUMS && cat SHA256SUMS )
echo
echo "完成：$DMG"
echo "Open Design 组件资产（须上传到 release ${A24_RELEASE_TAG}）：$OD_TARBALL"
