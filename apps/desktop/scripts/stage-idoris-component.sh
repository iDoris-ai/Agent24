#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
desktop="$(cd "$here/.." && pwd)"
repo="$(cd "$desktop/../.." && pwd)"
out="$desktop/.idoris-component"
cache="$desktop/.idoris-component-cache"
arch="${1:-$(uname -m)}"
case "$arch" in arm64) platform=darwin-arm64 ;; x86_64|x64) platform=darwin-x64 ;; *) echo "unsupported iDoris architecture: $arch" >&2; exit 1 ;; esac
meta="$(node -e 'const l=require(process.argv[1]); const a=l.assets[process.argv[2]]; if(!a||!a.name||!/^([a-f0-9]{64})$/.test(a.sha256)) process.exit(2); process.stdout.write(l.release_url_base+"/"+l.version+"\n"+a.name+"\n"+a.sha256)' "$repo/rust/crates/agent24-idoris/idoris.lock.json" "$platform")"
url="$(printf '%s\n' "$meta" | sed -n '1p')"; name="$(printf '%s\n' "$meta" | sed -n '2p')"; sha="$(printf '%s\n' "$meta" | sed -n '3p')"
mkdir -p "$cache"; asset="$cache/$name"
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
if [[ ! -f "$asset" || "$(hash "$asset")" != "$sha" ]]; then
  rm -f "$asset"; partial="$(mktemp "$cache/.partial.XXXXXX")"; trap 'rm -f "$partial"' EXIT
  curl -fL --retry 3 --retry-delay 2 -o "$partial" "$url/$name"
  [[ "$(hash "$partial")" == "$sha" ]] || { echo "iDoris archive sha256 mismatch" >&2; exit 1; }
  mv "$partial" "$asset"; trap - EXIT
fi
mkdir -p "$out"; cp -f "$asset" "$out/$name"
echo "staged iDoris $name into $out"
