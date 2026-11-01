#!/usr/bin/env bash
# 发布之后确认清单里的 Open Design 资产 URL 真的能下载。
# 用法：
#   bash apps/desktop/scripts/check-open-design-release-asset.sh <dmg 或清单 json>
# 读出清单的 url / sha256 / size，用 curl -fsIL --max-time 60 确认 URL 可达。
# 服务器如果返回了 Content-Length，再核对该值是否等于清单里的 size。
# 没有 Content-Length 时只确认可达，不把 size 当成失败。
set -euo pipefail

TARGET="${1:?usage: check-open-design-release-asset.sh <dmg-or-manifest-json>}"
MOUNT=""
cleanup() {
  if [[ -n "$MOUNT" ]]; then
    hdiutil detach "$MOUNT" >/dev/null || true
  fi
}
trap cleanup EXIT

if [[ "$TARGET" == *.dmg ]]; then
  [[ "$(uname -s)" == "Darwin" ]] || { echo "FAIL: reading a dmg requires macOS" >&2; exit 1; }
  ATTACH="$(hdiutil attach -readonly -nobrowse "$TARGET")"
  MOUNT="$(printf '%s\n' "$ATTACH" | sed -n 's#.*\(/Volumes/.*\)#\1#p' | head -n 1)"
  [[ -n "$MOUNT" ]] || { echo "FAIL: could not mount $TARGET" >&2; exit 1; }
  MANIFEST="$(find "$MOUNT" -name open-design-component.json -print -quit)"
  [[ -n "$MANIFEST" ]] || { echo "FAIL: dmg has no open-design-component.json" >&2; exit 1; }
elif [[ -f "$TARGET" ]]; then
  MANIFEST="$TARGET"
else
  echo "FAIL: not a file: $TARGET" >&2
  exit 1
fi

FIELDS="$(node -e '
const fs = require("fs");
const m = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (typeof m.url !== "string" || !/^https?:\/\//.test(m.url)) { console.error("manifest url is missing"); process.exit(1); }
if (typeof m.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(m.sha256)) { console.error("manifest sha256 is missing"); process.exit(1); }
if (typeof m.size !== "number" || !Number.isInteger(m.size) || m.size <= 0) { console.error("manifest size is missing"); process.exit(1); }
process.stdout.write(m.url + "\n" + m.sha256 + "\n" + String(m.size) + "\n");
' "$MANIFEST")"
URL="$(printf '%s\n' "$FIELDS" | sed -n '1p')"
SHA="$(printf '%s\n' "$FIELDS" | sed -n '2p')"
SIZE="$(printf '%s\n' "$FIELDS" | sed -n '3p')"

if ! HEADERS="$(curl -fsIL --max-time 60 "$URL")"; then
  echo "FAIL: asset URL is not reachable: $URL" >&2
  exit 1
fi

CL="$(printf '%s\n' "$HEADERS" | awk '
{
  line = $0
  sub(/\r$/, "", line)
  key = tolower(line)
  if (key ~ /^content-length:/) {
    sub(/^[^:]*:[[:space:]]*/, "", line)
    last = line
  }
}
END { print last }
')"
if [[ -n "$CL" && "$CL" != "$SIZE" ]]; then
  echo "FAIL: Content-Length $CL does not match manifest size $SIZE ($URL)" >&2
  exit 1
fi

if [[ -n "$CL" ]]; then
  echo "OK: $URL reachable, Content-Length $CL matches manifest size, sha256 $SHA"
else
  echo "OK: $URL reachable (no Content-Length; size not checked), sha256 $SHA"
fi
