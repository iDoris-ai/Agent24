#!/usr/bin/env bash
# 兼容 shell 入口：原样转发给只读观察器（-I 隔离模式，不读取 PYTHON* 环境与当前目录模块）。
exec python3 -I "$(dirname "$0")/artifact_observer.py" "$@"
