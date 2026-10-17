#!/usr/bin/env bash
# 显式入口：checker 自身单测。历史 RED 钉在不可变 git 对象上；工作树 RED 请用 run-history.sh。
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
home="$(mktemp -d)"; trap 'rm -rf "$home"' EXIT
cd "$root" && exec env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home/.config" TMPDIR="$home" \
  node --test scripts/contract-v1-hardware/checker.test.mjs
