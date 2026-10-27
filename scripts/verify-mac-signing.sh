#!/usr/bin/env bash
# 验证 macOS 签名/公证产物。用法：
#   scripts/verify-mac-signing.sh [--expect-notarized] [--arch arm64|x86_64|x64] <dmg|.app>
# dmg：先对 dmg 本身做 spctl（open 上下文）与 stapler 校验，再只读挂载 -> 复制 .app 到临时目录 -> 卸载 -> 对副本验签。
# 任一断言失败即非 0 退出。
set -euo pipefail

EXPECT_NOTARIZED=0
WANT_ARCH=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --expect-notarized) EXPECT_NOTARIZED=1; shift ;;
    --arch) WANT_ARCH="$2"; shift 2 ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) break ;;
  esac
done
TARGET="${1:-}"
[[ -n "$TARGET" && -e "$TARGET" ]] || { echo "用法：$0 [--expect-notarized] [--arch A] <dmg|.app>" >&2; exit 2; }
[[ "$WANT_ARCH" == "x64" ]] && WANT_ARCH="x86_64"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/agent24-verify.XXXXXX")"
MNT=""
cleanup() {
  [[ -n "$MNT" ]] && hdiutil detach "$MNT" -quiet 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
hr() { printf '\n--- %s ---\n' "$*"; }

APP=""
if [[ "$TARGET" == *.dmg ]]; then
  hr "dmg: spctl (open / primary-signature)"
  SP="$(spctl -a -t open --context context:primary-signature -v "$TARGET" 2>&1)" || { echo "$SP"; fail "dmg 未通过 Gatekeeper"; }
  echo "$SP"
  if [[ "$EXPECT_NOTARIZED" == 1 ]]; then
    grep -q "source=Notarized Developer ID" <<<"$SP" || fail "dmg 不是 Notarized Developer ID"
    hr "dmg: stapler validate"
    xcrun stapler validate "$TARGET"
  fi
  hr "dmg: codesign -dv"
  codesign -dv --verbose=2 "$TARGET" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Timestamp|flags" || true
  MNT="$WORK/mnt"; mkdir -p "$MNT"
  hdiutil attach -readonly -nobrowse -noverify -mountpoint "$MNT" "$TARGET" -quiet
  SRC_APP="$(find "$MNT" -maxdepth 1 -name '*.app' -print | head -n1)"
  [[ -d "$SRC_APP" ]] || fail "dmg 内没有 .app"
  mkdir -p "$WORK/copy"
  ditto "$SRC_APP" "$WORK/copy/$(basename "$SRC_APP")"
  hdiutil detach "$MNT" -quiet; MNT=""
  APP="$WORK/copy/$(basename "$SRC_APP")"
else
  APP="$TARGET"
fi

hr "app: codesign --verify --deep --strict"
codesign --verify --deep --strict --verbose=1 "$APP" 2>&1 || fail "codesign 校验失败"

hr "app: spctl -a -vv (execute)"
SP2="$(spctl -a -vv "$APP" 2>&1)" || { echo "$SP2"; fail "app 未通过 Gatekeeper"; }
echo "$SP2"
if [[ "$EXPECT_NOTARIZED" == 1 ]]; then
  # 副本无 staple 票据时 Gatekeeper 走在线查询；离线机器需要 dmg 上的 staple
  grep -q "source=Notarized Developer ID" <<<"$SP2" || fail "app 不是 Notarized Developer ID"
fi

hr "app: 主程序与嵌套二进制签名（需 Hardened Runtime: flags 含 runtime）"
BAD=0
check_bin() {
  local f="$1" out
  out="$(codesign -dv --verbose=4 "$f" 2>&1)" || { echo "UNSIGNED  $f"; BAD=1; return; }
  if grep -Eq "flags=0x[0-9a-f]+\(.*runtime" <<<"$out"; then
    echo "OK(runtime) ${f#"$APP"/}  $(grep -m1 TeamIdentifier <<<"$out")"
  else
    echo "NO-RUNTIME  ${f#"$APP"/}"; BAD=1
  fi
}
MAIN="$APP/Contents/MacOS/$(/usr/bin/plutil -extract CFBundleExecutable raw -o - "$APP/Contents/Info.plist")"
check_bin "$MAIN"
[[ -f "$APP/Contents/Resources/backend/agent24d" ]] || fail "缺少 Contents/Resources/backend/agent24d"
check_bin "$APP/Contents/Resources/backend/agent24d"
# 其余 Resources 下的 Mach-O（含 .node / 辅助二进制），逐个必须已签
while IFS= read -r f; do
  [[ "$f" == "$APP/Contents/Resources/backend/agent24d" ]] && continue
  if file -b "$f" | grep -q "Mach-O"; then
    codesign -dv "$f" >/dev/null 2>&1 || { echo "UNSIGNED  ${f#"$APP"/}"; BAD=1; }
  fi
done < <(find "$APP/Contents/Resources" -type f \( -perm -u+x -o -name '*.node' -o -name '*.dylib' \) 2>/dev/null)
[[ "$BAD" == 0 ]] || fail "存在未签名或未启用 Hardened Runtime 的二进制"

hr "app: lipo -archs"
APP_ARCH="$(lipo -archs "$MAIN")"; D_ARCH="$(lipo -archs "$APP/Contents/Resources/backend/agent24d")"
echo "主程序: $APP_ARCH   agent24d: $D_ARCH"
[[ "$APP_ARCH" == "$D_ARCH" ]] || fail "主程序与 agent24d 架构不一致"
if [[ -n "$WANT_ARCH" ]]; then
  [[ "$APP_ARCH" == "$WANT_ARCH" ]] || fail "期望架构 ${WANT_ARCH}，实际 $APP_ARCH"
fi
if [[ "$TARGET" == *.dmg ]]; then
  case "$(basename "$TARGET")" in
    *arm64*) [[ "$APP_ARCH" == "arm64" ]] || fail "dmg 文件名为 arm64，但 app 为 $APP_ARCH" ;;
    *x64*)   [[ "$APP_ARCH" == "x86_64" ]] || fail "dmg 文件名为 x64，但 app 为 $APP_ARCH" ;;
  esac
fi

echo; echo "PASS: 签名验证通过"
