#!/usr/bin/env bash
# 由 SVG 生成桌面端用到的图标。需要 Playwright：NODE_PATH=<playwright 所在 node_modules> bash render.sh
set -euo pipefail
cd "$(dirname "$0")/../../.."
B=docs/brand; A=apps/desktop/assets
r() { node $B/src/render.cjs "$1" "$2" "$3"; }
T=$(mktemp -d); IS=$T/icon.iconset; mkdir -p $IS
# 32px 及以下用不带数字的小尺寸版（VI.md §3.2）
src() { if [ "$1" -le 32 ]; then echo $B/app-icon-small.svg; else echo $B/app-icon.svg; fi; }
for s in 16 32 128 256 512; do
  r $(src $s) $IS/icon_${s}x${s}.png $s
  r $(src $((s*2))) $IS/icon_${s}x${s}@2x.png $((s*2))
done
iconutil -c icns $IS -o $A/icon.icns
cp $IS/icon_512x512.png $A/icon.png
cp $IS/icon_512x512@2x.png $A/logo-square-1024.png
r $B/tray-template.svg $A/trayTemplate@2x.png 32
r $B/tray-template.svg $A/trayTemplate.png 16
echo done
