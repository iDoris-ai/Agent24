#!/usr/bin/env bash
# Stages the pinned Sin90 release into apps/desktop/.sin90-module/, which
# package.json's mac extraResources copies to
# Agent24.app/Contents/Resources/sin90-module/.
#
# The published tarball carries files.json hashed against the publisher's
# Mach-O bytes. electron-builder re-signs every Mach-O in the app
# (codesign --force), so those hashes would not match. This script drops
# files.json. At first launch the app checks bin/sin90's signature (team
# HYAP444FN9 and Hardened Runtime, the same rule as the documents module),
# then generates integrity metadata on a private temporary copy
# (main/documents-module.ts, SIN90_OS).
#
# Usage: stage-sin90-module.sh [arm64|x64]
#   default arch: uname -m (arm64, or x86_64 → x64).
# A verified tarball in apps/desktop/.sin90-module-cache/ is reused.
# macOS only.
#
# A mac build that never ran this ships no Resources/sin90-module/ at all:
# electron-builder only warns when an extraResources source is missing.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
desktop="$(cd "$here/.." && pwd)"
pin="$here/sin90-pin.json"
out="$desktop/.sin90-module"
cache="$desktop/.sin90-module-cache"

die() { echo "stage-sin90-module: $*" >&2; exit 1; }

[[ "$(uname -s)" == Darwin ]] || die "macOS only"
[[ -f "$pin" ]] || die "missing $pin"

arch="${1:-}"
if [[ -z "$arch" ]]; then
  case "$(uname -m)" in
    arm64) arch=arm64 ;;
    x86_64) arch=x64 ;;
    *) die "unsupported machine $(uname -m); pass arm64 or x64" ;;
  esac
fi
[[ "$arch" == arm64 || "$arch" == x64 ]] || die "arch must be arm64 or x64 (got: $arch)"

meta="$(node -e '
const fs = require("fs")
const pin = JSON.parse(fs.readFileSync(process.argv[1], "utf8"))
const arch = process.argv[2]
const asset = pin.assets && pin.assets[arch]
if (!pin.version || !asset || !asset.name || !/^[a-f0-9]{64}$/.test(asset.sha256 || "")) {
  console.error("stage-sin90-module: pin is missing version or " + arch)
  process.exit(1)
}
process.stdout.write(pin.version + "\n" + asset.name + "\n" + asset.sha256 + "\n")
' "$pin" "$arch")" || die "could not read $pin"
version="$(printf '%s\n' "$meta" | sed -n '1p')"
name="$(printf '%s\n' "$meta" | sed -n '2p')"
sha="$(printf '%s\n' "$meta" | sed -n '3p')"
[[ -n "$version" && -n "$name" && -n "$sha" ]] || die "empty pin fields"

url="https://github.com/iDoris-ai/Sin90/releases/download/v${version}/${name}"
mkdir -p "$cache"
asset="$cache/$name"

sha_of() { shasum -a 256 "$1" | awk '{print $1}'; }

if [[ -f "$asset" ]] && [[ "$(sha_of "$asset")" == "$sha" ]]; then
  echo "stage-sin90-module: using cached $asset"
else
  rm -f "$asset"
  partial="$(mktemp "$cache/.partial.XXXXXX")"
  trap 'rm -f "$partial"' EXIT
  curl -fL --retry 3 --retry-delay 2 -o "$partial" "$url" \
    || die "download failed: $url"
  got="$(sha_of "$partial")"
  if [[ "$got" != "$sha" ]]; then
    rm -f "$partial"
    trap - EXIT
    die "sha256 mismatch for $name: expected $sha got $got"
  fi
  mv "$partial" "$asset"
  trap - EXIT
fi

extract="$(mktemp -d "$desktop/.sin90-module.tmp.XXXXXX")"
trap 'rm -rf "$extract"' EXIT
# System tar keeps the published modes (directories 0700, files 0400 or 0500).
tar -xf "$asset" -C "$extract"
tops=()
while IFS= read -r d; do
  [[ -n "$d" ]] && tops+=("$d")
done < <(find "$extract" -mindepth 1 -maxdepth 1 -type d)
[[ "${#tops[@]}" -eq 1 ]] || die "expected one top-level directory in $name (found ${#tops[@]})"
pkg="${tops[0]}"
[[ -f "$pkg/domain-os.yml" && -f "$pkg/bin/sin90" ]] || die "tarball layout is not domain-os.yml + bin/sin90"
# Drop the publisher's files.json. The re-signed bin/sin90 will not match it.
rm -f "$pkg/files.json"
[[ ! -e "$pkg/files.json" ]] || die "could not remove files.json"

rm -rf "$out"
mv "$pkg" "$out"
trap - EXIT
rm -rf "$extract"
echo "staged sin90 ${version} (${arch}) into $out"
