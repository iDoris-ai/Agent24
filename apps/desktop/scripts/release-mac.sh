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
set -euo pipefail

SIGN_IDENTITY="${AGENT24_SIGN_IDENTITY:-Developer ID Application: Huifeng Jiao (HYAP444FN9)}"
NOTARY_PROFILE="${AGENT24_NOTARY_PROFILE:-apple-notary-HYAP444FN9}"
ARCH="${AGENT24_ARCH:-arm64}"

DESKTOP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$DESKTOP_DIR/../.." && pwd)"
VERIFY="$REPO_ROOT/scripts/verify-mac-signing.sh"

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
# electron-builder 的 identity 不能带 "Developer ID Application:" 前缀
BUILDER_IDENTITY="${SIGN_IDENTITY#Developer ID Application: }"
# S3：cargo 产物路径按 <target>/release 读取，下面两种环境会导致读到陈旧产物或解析错目录
[[ -z "${CARGO_BUILD_TARGET:-}" ]] || die "请 unset CARGO_BUILD_TARGET：设了它产物在 <target>/<triple>/release，脚本会读到陈旧产物"
[[ -z "${CARGO_TARGET_DIR:-}" || "${CARGO_TARGET_DIR}" == /* ]] || die "CARGO_TARGET_DIR 必须是绝对路径（当前：${CARGO_TARGET_DIR}）"
# 防止环境里的 CSC_* 把签名悄悄切到别的证书 / 或让 electron-builder 去找 p12
unset CSC_LINK CSC_KEY_PASSWORD CSC_NAME

step "1/6 构建 Rust release（agent24d、agent24）"
( cd "$REPO_ROOT" && cargo build --release -p agent24d -p agent24-cli --manifest-path rust/Cargo.toml )
CARGO_OUT="${CARGO_TARGET_DIR:-$REPO_ROOT/rust/target}/release"
[[ -x "$CARGO_OUT/agent24d" ]] || die "缺少 $CARGO_OUT/agent24d"
if [[ "$CARGO_OUT" != "$REPO_ROOT/rust/target/release" ]]; then
  mkdir -p "$REPO_ROOT/rust/target/release"
  cp -f "$CARGO_OUT/agent24d" "$REPO_ROOT/rust/target/release/agent24d"
fi
# 每次都从未签名的干净二进制起步，避免 electron-builder 对已签名文件重复处理
codesign --remove-signature "$REPO_ROOT/rust/target/release/agent24d" 2>/dev/null || true

step "2/6 构建 node-daemon 与桌面端前端/主进程"
cd "$DESKTOP_DIR"
pnpm --filter @agent24/node-daemon build
pnpm build

step "3/6 electron-builder 打包 + 签名（${ARCH}，Hardened Runtime）"
rm -rf "$DESKTOP_DIR/release"
pnpm exec electron-builder --mac "--${ARCH}" -c.mac.identity="$BUILDER_IDENTITY"
DMG="$(find "$DESKTOP_DIR/release" -maxdepth 1 -name '*.dmg' -print | head -n1)"
[[ -f "$DMG" ]] || die "未生成 dmg"
echo "dmg: $DMG"

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
