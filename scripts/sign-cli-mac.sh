#!/usr/bin/env bash
# CLI / 模块裸二进制签名 + 公证（DEP-B3）。
# 用法：scripts/sign-cli-mac.sh [-o OUT_DIR] [BINARY ...]
#   不带 BINARY 时处理 $CARGO_TARGET_DIR/release（默认 rust/target/release）下的 agent24 与 agent24d。
#   也可显式传入 bin/sin90、bin/cos72 等任意 Mach-O 路径。
# 环境变量：AGENT24_SIGN_IDENTITY / AGENT24_NOTARY_PROFILE（默认同 release-mac.sh）、SKIP_NOTARIZE=1。
# 产物（OUT_DIR，默认 dist/mac-cli；dist/ 已在 .gitignore，勿提交）：
#   <name> 已签名二进制、agent24-cli-macos-<arch>.zip、notarize-submission.json、SHA256SUMS
# 说明：裸 Mach-O 无法 staple（票据只能附着在 .app/.pkg/.dmg 上）；Gatekeeper 首次运行时在线查询公证票据。
#       因此这里记录 submission id，并在文档中说明离线机器的限制。
set -euo pipefail

SIGN_IDENTITY="${AGENT24_SIGN_IDENTITY:-Developer ID Application: Huifeng Jiao (HYAP444FN9)}"
NOTARY_PROFILE="${AGENT24_NOTARY_PROFILE:-apple-notary-HYAP444FN9}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/dist/mac-cli"
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

while getopts "o:h" o; do
  case "$o" in o) OUT="$OPTARG" ;; *) sed -n '2,9p' "$0"; exit 2 ;; esac
done
shift $((OPTIND - 1))

[[ "$(uname -s)" == "Darwin" ]] || die "只能在 macOS 上运行"
security find-identity -v -p codesigning | grep -F "$SIGN_IDENTITY" >/dev/null \
  || die "keychain 中找不到签名身份：$SIGN_IDENTITY"
if [[ "${SKIP_NOTARIZE:-0}" != "1" ]]; then
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null \
    || die "notarytool profile 不可用：$NOTARY_PROFILE"
fi

BINS=("$@")
if [[ ${#BINS[@]} -eq 0 ]]; then
  REL="${CARGO_TARGET_DIR:-$REPO_ROOT/rust/target}/release"
  BINS=("$REL/agent24" "$REL/agent24d")
fi

rm -rf "$OUT"; mkdir -p "$OUT/stage"
ARCH=""
NAMES=()
for b in "${BINS[@]}"; do
  [[ -f "$b" ]] || die "找不到二进制：$b"
  file -b "$b" | grep -q "Mach-O" || die "不是 Mach-O：$b"
  n="$(basename "$b")"
  cp -f "$b" "$OUT/stage/$n"
  # --force 覆盖 cargo/ld 附带的 ad-hoc 签名；runtime = Hardened Runtime；--timestamp = 安全时间戳（公证必需）
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$OUT/stage/$n"
  codesign --verify --strict --verbose=1 "$OUT/stage/$n"
  codesign -dv "$OUT/stage/$n" 2>&1 | grep -E "flags|TeamIdentifier|Timestamp" || true
  a="$(lipo -archs "$OUT/stage/$n")"
  [[ -z "$ARCH" ]] && ARCH="$a"
  [[ "$a" == "$ARCH" ]] || die "架构不一致：$n=${a}，其他=$ARCH"
  NAMES+=("$n")
done

ZIP="$OUT/agent24-cli-macos-$ARCH.zip"
# zip 内直接放二进制（无多余目录层）；ditto 保留元数据，notarytool 接受 zip
rm -f "$ZIP"
( cd "$OUT/stage" && ditto -c -k . "$ZIP" )

if [[ "${SKIP_NOTARIZE:-0}" != "1" ]]; then
  SUBMIT="$OUT/notarize-submission.json"
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json | tee "$SUBMIT"
  STATUS="$(/usr/bin/plutil -extract status raw -o - "$SUBMIT")"
  SUB_ID="$(/usr/bin/plutil -extract id raw -o - "$SUBMIT")"
  if [[ "$STATUS" != "Accepted" ]]; then
    xcrun notarytool log "$SUB_ID" --keychain-profile "$NOTARY_PROFILE" || true
    die "公证未通过：status=$STATUS id=$SUB_ID"
  fi
  echo "notarization submission id: $SUB_ID"
else
  echo "已跳过公证（SKIP_NOTARIZE=1）"
fi

mv "$OUT/stage/"* "$OUT/"; rmdir "$OUT/stage"
( cd "$OUT" && shasum -a 256 "${NAMES[@]}" "$(basename "$ZIP")" > SHA256SUMS && cat SHA256SUMS )
echo "产物目录：$OUT"
