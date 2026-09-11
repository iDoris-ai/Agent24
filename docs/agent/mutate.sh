#!/usr/bin/env bash
# 变异测试脚手架 —— 薄包装。所有判断、恢复、加锁都在 `mutate.py` 里；为什么见那个文件的头。
# 它自己先要被验过：`bash docs/agent/mutate-selftest.sh`。
#
# 用法（bash；source 它不改调用方 shell 的选项，也不留下它自己的 trap）：
#   source docs/agent/mutate.sh
#   mut_baseline <crate> [测试过滤…]        # 先立基线,拿到它才允许开始
#   mut <文件> <锚点> <替换> <标签>          # 之后 $_MUT_LAST = RED / ALIVE / VOID / TIMEOUT
#   mut_recover                             # 上一次被 SIGKILL 打断时,把被变异的文件恢复
#
# 返回值：一格读完(不论读成什么)返回 0；拒绝开始返回 1；源码恢复失败返回 2；被信号 n 打断
# 返回 128+n —— 非交互 shell(脚本)里直接以它退出,交互式 shell 里只返回,提示符留着。
# `MUT_TIMEOUT`（秒,默认 150）与 `MUT_CARGO`（默认 cargo；自证用它换成假 cargo）可覆盖。

_MUT_PY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/mutate.py"
_MUT_LAST=""

# 交互式：前台跑，Ctrl-C 由终端直接交给 python（它在自己的进程组里）。
# 非交互：后台跑再 wait，把发给本 shell 的信号转给 python —— 否则 `kill <脚本>` 只杀掉
# shell，python 成了孤儿，照样跑完、照样恢复,但不再是「立刻停」。转发 trap 只做转发:
# 源码安全不依赖它 —— 那由 python 保证,这里没有任何 trap 窗口能留下被变异的文件。
_mut_py() {
  if [[ $- == *i* ]]; then python3 "$_MUT_PY" "$@"; return $?; fi
  local saved pid rc s
  saved=$(trap -p HUP INT QUIT TERM)
  python3 "$_MUT_PY" "$@" &
  pid=$!
  # shellcheck disable=SC2064 # pid 在此刻展开
  for s in HUP INT QUIT TERM; do trap "kill -s $s $pid 2>/dev/null" "$s"; done
  wait "$pid"; rc=$?
  # trap 触发会让 wait 提前返回;python 还活着就继续等它收尾。
  while kill -0 "$pid" 2>/dev/null; do wait "$pid"; rc=$?; done
  trap - HUP INT QUIT TERM
  [ -n "$saved" ] && eval "$saved"
  return "$rc"
}

_mut_done() { # 被信号打断:脚本里就此退出(和被那个信号杀掉一样),交互式里只返回
  local rc=$1
  if [ "$rc" -ge 128 ] && [[ $- != *i* ]]; then exit "$rc"; fi
  return "$rc"
}

mut_baseline() { # mut_baseline <crate> [过滤…]
  _mut_py baseline "$@"
  _mut_done $?
}

mut() { # mut <文件> <锚点> <替换> <标签>
  _MUT_LAST=""
  _mut_py mut "$@"
  local rc=$?
  case $rc in
    0) _MUT_LAST=RED ;; 10) _MUT_LAST=ALIVE ;; 20) _MUT_LAST=VOID ;; 30) _MUT_LAST=TIMEOUT ;;
  esac
  case $rc in 0 | 10 | 20 | 30) return 0 ;; esac
  _mut_done "$rc"
}

mut_recover() {
  _mut_py recover
  _mut_done $?
}
