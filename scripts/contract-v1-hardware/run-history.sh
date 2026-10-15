#!/usr/bin/env bash
# 显式入口（不绑定默认 CI）：历史基准 6072fd37 → 当前 protocol/openapi.yaml。
# 在 a9b 上预期 RED（exit 1，required free_disk_bytes 被删）；恢复候选应使其 exit 0。
# 用法: scripts/contract-v1-hardware/run-history.sh [新 openapi 路径，默认 protocol/openapi.yaml]
set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
new="${1:-protocol/openapi.yaml}"
home="$(mktemp -d)"
trap 'rm -rf "$home"' EXIT
cd "$root" && env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home/.config" TMPDIR="$home" \
  node scripts/contract-v1-hardware/cli.mjs \
  --old git:6072fd373142eb3dbf1a6a4a3222e6eec82c74f6:protocol/openapi.yaml --new "$new"
