#!/usr/bin/env bash
# macOS 版「Open Design 不进包、只带清单」验证（对应 release-desktop.yml 里 Linux 的同名步骤）。
# 用法：verify-mac-open-design.sh <Agent24.app> <dmg> <tarball-dir> <app-version>
#   app-version = 发布 tag 去掉前导 v（与清单里的下载 URL 对应）
# 检查：
#   1. Contents/Resources/open-design-component.json 存在，且 version/odSha/platform/arch/url 正确
#   2. Resources 里没有 app/ open-design/ open-design-web-standalone/ 目录，也没有任何 *.tar.gz（C1）
#   3. 清单里的 sha256/size 与 tarball-dir 里那个资产文件实际一致
#   4. DMG 不超过 A24_DMG_MAX_MB（默认 250），防止 Open Design 被重新打进安装包
#   5. 解开 tarball，对每个 Mach-O 跑 codesign --verify --strict，TeamIdentifier 必须是 HYAP444FN9
#      （SKIP_NOTARIZE=1 时同样执行；这一步不依赖公证）
set -euo pipefail

APP="${1:?usage: verify-mac-open-design.sh <Agent24.app> <dmg> <tarball-dir> <app-version>}"
DMG="${2:?missing dmg}"
TARBALL_DIR="${3:?missing tarball dir}"
APP_VERSION="${4:?missing app version}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RES="$APP/Contents/Resources"
MANIFEST="$RES/open-design-component.json"
MAX_MB="${A24_DMG_MAX_MB:-250}"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
# BSD stat -f 和 GNU stat -c 写法不同。wc -c 在两边都是字节数。
file_bytes() { wc -c < "$1" | tr -d '[:space:]'; }
field() { node -e "process.stdout.write(String(JSON.parse(require('fs').readFileSync(process.argv[1],'utf8'))[process.argv[2]]))" "$MANIFEST" "$1"; }

[[ -f "$MANIFEST" ]] || fail "包内缺少 ${MANIFEST}（Design 页会显示『组件未安装』）"
PIN_SHA="$(node -e "import('$SCRIPT_DIR/pack-open-design-component.mjs').then(m=>process.stdout.write(m.OPEN_DESIGN_SHA))")"
PIN_VERSION="$(node -e "import('$SCRIPT_DIR/pack-open-design-component.mjs').then(m=>process.stdout.write(m.OPEN_DESIGN_PIN_VERSION))")"
[[ "$(field version)" == "$PIN_VERSION" ]] || fail "manifest.version=$(field version)（期望 $PIN_VERSION）"
[[ "$(field odSha)" == "$PIN_SHA" ]] || fail "manifest.odSha 与固定 sha 不符：$(field odSha)"
[[ "$(field platform)" == "darwin" ]] || fail "manifest.platform=$(field platform)"
[[ "$(field arch)" == "arm64" ]] || fail "manifest.arch=$(field arch)"
URL="$(field url)"
ASSET="$(basename "$URL")"
[[ "$URL" == "https://github.com/iDoris-ai/Agent24/releases/download/v${APP_VERSION}/${ASSET}" ]] \
  || fail "manifest.url 必须是 iDoris-ai/Agent24 上的 tag v${APP_VERSION}：$URL"

for tree in app open-design open-design-web-standalone; do
  [[ ! -e "$RES/$tree" ]] || fail "Open Design 目录被打进了安装包：$RES/$tree"
done
if find "$RES" -maxdepth 2 -name '*.tar.gz' -print | grep -q .; then
  find "$RES" -maxdepth 2 -name '*.tar.gz' >&2
  fail "安装包 Resources 内发现 .tar.gz —— 按需组件绝不能进安装包"
fi

TARBALL="$TARBALL_DIR/$ASSET"
[[ -f "$TARBALL" ]] || fail "找不到与清单对应的资产文件：$TARBALL"
[[ "$(shasum -a 256 "$TARBALL" | awk '{print $1}')" == "$(field sha256)" ]] || fail "资产 sha256 与清单不一致"
[[ "$(file_bytes "$TARBALL")" == "$(field size)" ]] || fail "资产 size 与清单不一致"

DMG_BYTES="$(file_bytes "$DMG")"
(( DMG_BYTES <= MAX_MB * 1024 * 1024 )) || fail "$(basename "$DMG") 为 ${DMG_BYTES} 字节，超过 ${MAX_MB}MB 上限 —— Open Design 可能被重新打进安装包"

# 公证与否都要做：解开 tarball，每个 Mach-O codesign --verify --strict，Team ID 必须是 HYAP444FN9。
# A24_OD_SKIP_CODESIGN=1 只给行为测试：假 tarball 没有已签名 Mach-O，真实 codesign
# 会因夹具失败，盖住清单 / 目录 / sha 这些断言。release-mac.sh 不设置它。
EXTRACT="$(mktemp -d "${TMPDIR:-/tmp}/a24-od-verify.XXXXXX")"
cleanup_extract() { rm -rf "$EXTRACT"; }
trap cleanup_extract EXIT
tar -xzf "$TARBALL" -C "$EXTRACT"
if [[ "${A24_OD_SKIP_CODESIGN:-}" == "1" ]]; then
  MACH_O_NOTE="Mach-O codesign 已跳过（A24_OD_SKIP_CODESIGN=1）"
else
  node "$SCRIPT_DIR/open-design-mac-release.mjs" verify-signed "$EXTRACT" \
    || fail "Open Design tarball 里有 Mach-O 未通过 codesign --verify --strict，或 TeamIdentifier 不是 HYAP444FN9"
  MACH_O_NOTE="tarball 内 Mach-O 已验证 Team ID"
fi

echo "OK: manifest 在包内（url=${URL}），包内无 Open Design 目录/tarball，资产 sha256/size 一致，DMG=${DMG_BYTES} 字节（上限 ${MAX_MB}MB），${MACH_O_NOTE}"
