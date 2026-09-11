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
# 返回 128+n。非交互 shell(脚本)里,收到的信号会交还给本 shell:调用方有自己的 trap 就照常
# 运行它;没有,本 shell(含 `( … )` 子 shell)就**死于这个信号**,调用方的 EXIT trap 照常运行 ——
# 外层的批量循环因此也会停下。交互式 shell 里只返回,提示符留着。
# 对 `set -e` 安全:一格读成 ALIVE/VOID 不会让调用方脚本退出。
# `MUT_TIMEOUT`（秒,默认 150）与 `MUT_CARGO`（默认 cargo；自证用它换成假 cargo）可覆盖。

_MUT_PY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/mutate.py"
_MUT_LAST=""

# 交互式：前台跑，Ctrl-C 由终端直接交给 python（它在自己的进程组里）。
# 非交互：后台跑再 wait，把发给本 shell 的信号转给 python —— 否则 `kill <脚本>` 只杀掉
# shell，python 成了孤儿，照样跑完、照样恢复,但不再是「立刻停」。转发 trap 只做转发:
# 源码安全不依赖它 —— 那由 python 保证,这里没有任何 trap 窗口能留下被变异的文件。
_mut_py() {
  local rc=0
  _MUT_SIG=""
  # `|| rc=$?` 而不是裸调用:调用方开着 `set -e` 时,一个 ALIVE(10)/VOID(20) 读数不能把
  # 整个脚本带走 —— 那不是失败,是一格读完了。
  if [[ $- == *i* ]]; then python3 "$_MUT_PY" "$@" || rc=$?; return "$rc"; fi
  local s
  _MUT_SAVED=$(trap -p HUP INT QUIT TERM)
  # trap 先装、再起 python:反过来的话,两步之间到的信号会按调用方的默认处置杀掉 shell,
  # python 成了孤儿。起之前到的信号先记下,起之后立刻转过去。
  _MUT_PID=""
  # shellcheck disable=SC2064
  for s in HUP INT QUIT TERM; do trap "_MUT_SIG=$s; [ -n \"\$_MUT_PID\" ] && kill -s $s \$_MUT_PID 2>/dev/null" "$s"; done
  python3 "$_MUT_PY" "$@" &
  _MUT_PID=$!
  [ -n "$_MUT_SIG" ] && kill -s "$_MUT_SIG" "$_MUT_PID" 2>/dev/null
  local pid=$_MUT_PID
  wait "$pid" || rc=$?
  # trap 触发会让 wait 提前返回;python 还活着就继续等它收尾。
  while kill -0 "$pid" 2>/dev/null; do rc=0; wait "$pid" || rc=$?; done
  trap - HUP INT QUIT TERM
  [ -n "$_MUT_SAVED" ] && eval "$_MUT_SAVED"
  return "$rc"
}

_mut_done() { # 收到过信号:把它原样交还给本 shell —— 调用方自己的 trap 照常运行;没有 trap 就按默认处置退出
  local rc=$1
  if [ -n "${_MUT_SIG:-}" ] && [[ $- != *i* ]]; then
    local sig=$_MUT_SIG
    _MUT_SIG=""
    if [ -n "$(trap -p "$sig")" ]; then
      # 调用方对这个信号有自己的 trap:交还信号,让它照常运行。发给「本 shell」:bash 3.2
      # 没有 $BASHPID,$$ 在子 shell 里是父 shell;让 sh 直接 kill 它的父进程 —— sh 是本
      # shell 的直接子进程。trap 运行了且没退出,那是调用方的决定。
      sh -c 'kill -s "$1" "$PPID"' _ "$sig"
      return "$rc"
    fi
    # 调用方没有 trap:**死于这个信号本身**,而不是 `exit 128+n`。区别是给上一层看的:
    # 等着本 shell 的父 bash 也收到了同一个 Ctrl-C,它按 wait-and-cooperative-exit 规则,
    # 只在子进程「死于 SIGINT」时才跟着退出 —— `exit 130` 是正常退出,父 bash 会接着跑
    # 下一轮(复审 @ c78db3d F-A:外层批量循环里按 Ctrl-C 停不下)。
    # 「本 shell」要取对:bash 3.2 没有 $BASHPID,$$ 在 `( … )` 里是顶层脚本;
    # `$(exec sh -c 'echo $PPID')` 里命令替换的子 shell 被 sh 取代,sh 的父进程就是本 shell。
    # 调用方的 EXIT trap 照常运行(bash 死于它捕获的致命信号前会跑 EXIT trap;实测)。
    # 源码没恢复时,提示已经打印过;这里仍然死于信号 —— 让这一批停下比保留退出码 2 更要紧。
    local self
    self=$(exec sh -c 'echo "$PPID"')
    trap - "$sig"
    kill -s "$sig" "$self"
    # 信号是异步投递的;万一它在本 shell 启动时就被忽略(无法改回),退回到报告退出码。
    sleep 1
    exit $((128 + $(kill -l "$sig")))
  fi
  if [ "$rc" -ge 128 ] && [[ $- != *i* ]]; then exit "$rc"; fi
  return "$rc"
}

mut_baseline() { # mut_baseline <crate> [过滤…]
  local rc=0
  _mut_py baseline "$@" || rc=$?
  _mut_done "$rc"
}

mut() { # mut <文件> <锚点> <替换> <标签>
  local rc=0
  _MUT_LAST=""
  _mut_py mut "$@" || rc=$?
  case $rc in
    0) _MUT_LAST=RED ;; 10) _MUT_LAST=ALIVE ;; 20) _MUT_LAST=VOID ;; 30) _MUT_LAST=TIMEOUT ;;
  esac
  case $rc in 0 | 10 | 20 | 30) [ -z "${_MUT_SIG:-}" ] && return 0 ;; esac
  _mut_done "$rc"
}

mut_recover() {
  local rc=0
  _mut_py recover || rc=$?
  _mut_done "$rc"
}
