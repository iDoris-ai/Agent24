#!/usr/bin/env bash
# 发布之后确认清单里的 Open Design 资产 URL 真的能下载，并且字节与清单一致。
# 用法：
#   bash apps/desktop/scripts/check-open-design-release-asset.sh <dmg 或清单 json>
# GET 下载到临时文件，计算 sha256 和字节数，与清单比对，退出时删掉临时文件。
# 只接受 https。缺 Content-Length 不算失败：size 以实际下载的字节数为准。
#
# 单元测试的夹具是 Node 的 http.Server，签不出可用的 TLS 证书。因此字面量
# http://127.0.0.1 和 http://[::1] 有豁免，其它 http（含 127.0.0.1.evil.com、带用户信息的 URL）拒绝。
# 选择豁免而不是注入 curl：测试要走真实的下载、哈希和清理路径，替身 curl 盖不住这些。
# 重定向逐跳重查：后续跳转仍须是 https，或仍停在字面 loopback 的 http 上。
set -euo pipefail

TARGET="${1:?usage: check-open-design-release-asset.sh <dmg-or-manifest-json>}"
MOUNT=""
TMP=""
HDR=""
cleanup() {
  # bash 3.2 把 EXIT trap 的返回码当成脚本退出码。空变量上的 [[ ]] && rm
  # 会留下状态 1，把已经打印 OK 的成功跑成失败。
  if [[ -n "$MOUNT" ]]; then
    hdiutil detach "$MOUNT" >/dev/null || true
  fi
  if [[ -n "$TMP" ]]; then rm -f "$TMP"; fi
  if [[ -n "$HDR" ]]; then rm -f "$HDR"; fi
  return 0
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
let parsed;
try { parsed = new URL(m.url); }
catch { console.error("manifest url is missing"); process.exit(1); }
const host = parsed.hostname;
const loopback = host === "127.0.0.1" || host === "[::1]" || host === "::1";
const loopbackHttp = parsed.protocol === "http:" && loopback;
if (parsed.username || parsed.password) {
  console.error("manifest url must not contain credentials");
  process.exit(1);
}
if (parsed.protocol !== "https:" && !loopbackHttp) {
  console.error("manifest url must be https (http is only allowed for a literal loopback test server)");
  process.exit(1);
}
if (typeof m.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(m.sha256)) { console.error("manifest sha256 is missing"); process.exit(1); }
if (typeof m.size !== "number" || !Number.isInteger(m.size) || m.size <= 0) { console.error("manifest size is missing"); process.exit(1); }
process.stdout.write(m.url + "\n" + m.sha256 + "\n" + String(m.size) + "\n");
' "$MANIFEST")"
URL="$(printf '%s\n' "$FIELDS" | sed -n '1p')"
SHA="$(printf '%s\n' "$FIELDS" | sed -n '2p')"
SIZE="$(printf '%s\n' "$FIELDS" | sed -n '3p')"

url_is_allowed() {
  node -e '
    let parsed;
    try { parsed = new URL(process.argv[1]); }
    catch { process.exit(1); }
    const host = parsed.hostname;
    const loopback = host === "127.0.0.1" || host === "[::1]" || host === "::1";
    if (parsed.protocol === "https:") process.exit(0);
    if (parsed.protocol === "http:" && loopback && !parsed.username && !parsed.password) process.exit(0);
    process.exit(1);
  ' "$1"
}

TMP="$(mktemp "${TMPDIR:-/tmp}/a24-od-asset.XXXXXX")"
CURRENT="$URL"
for HOP in 1 2 3 4 5; do
  url_is_allowed "$CURRENT" || { echo "FAIL: asset URL must be https (http is only allowed for a literal loopback test server): $CURRENT" >&2; exit 1; }
  HDR="$(mktemp "${TMPDIR:-/tmp}/a24-od-asset-hdr.XXXXXX")"
  PROTO=(--proto '=https')
  case "$CURRENT" in
    http://127.0.0.1[:/]*|http://\[::1\][:/]*) PROTO=(--proto '=http,https') ;;
  esac
  if ! curl -fsS --max-time 60 "${PROTO[@]}" -D "$HDR" -o "$TMP" "$CURRENT"; then
    echo "FAIL: asset URL is not reachable: $CURRENT" >&2
    exit 1
  fi
  STATUS="$(awk 'BEGIN { code = "" } { line = $0; sub(/\r$/, "", line); if (line ~ /^HTTP\//) code = $2 } END { print code }' "$HDR")"
  case "$STATUS" in
    301|302|303|307|308)
      LOC="$(awk 'BEGIN { loc = "" } { line = $0; sub(/\r$/, "", line); key = tolower(line); if (key ~ /^location:/) { sub(/^[^:]*:[[:space:]]*/, "", line); loc = line } } END { print loc }' "$HDR")"
      rm -f "$HDR"
      HDR=""
      [[ -n "$LOC" ]] || { echo "FAIL: redirect is missing a Location header: $CURRENT" >&2; exit 1; }
      CURRENT="$(node -e 'process.stdout.write(new URL(process.argv[1], process.argv[2]).href)' "$LOC" "$CURRENT")"
      if [[ "$HOP" -eq 5 ]]; then
        echo "FAIL: too many redirects: $URL" >&2
        exit 1
      fi
      ;;
    *)
      rm -f "$HDR"
      HDR=""
      break
      ;;
  esac
done

ACTUAL_SHA="$(shasum -a 256 "$TMP" | awk '{print $1}')"
ACTUAL_SIZE="$(wc -c < "$TMP" | tr -d '[:space:]')"
if [[ "$ACTUAL_SIZE" != "$SIZE" ]]; then
  echo "FAIL: downloaded size $ACTUAL_SIZE does not match manifest size $SIZE ($URL)" >&2
  exit 1
fi
if [[ "$ACTUAL_SHA" != "$SHA" ]]; then
  echo "FAIL: downloaded sha256 $ACTUAL_SHA does not match manifest sha256 $SHA ($URL)" >&2
  exit 1
fi
echo "OK: $URL reachable, downloaded size $ACTUAL_SIZE matches manifest, sha256 $SHA verified"
