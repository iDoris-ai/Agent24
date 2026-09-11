#!/usr/bin/env bash
# 变异测试脚手架 —— 它自己先要被验过（`bash docs/agent/mutate-selftest.sh`）。
#
# 用法（bash；source 它不会改动调用方 shell 的选项）：
#   source docs/agent/mutate.sh
#   mut_baseline <crate> [测试过滤]        # 先立基线,拿到它才允许开始
#   mut <文件> <锚点> <替换> <标签>
#
# 每次 `mut` 之后 `$_MUT_LAST` 是这一格的判定：RED / ALIVE / VOID / TIMEOUT。
# 前置条件不满足（没有可用基线、文件不存在、文件不在基线那个 crate 里）→ 返回 1，不跑。
# `MUT_TIMEOUT`（秒，默认 150）与 `MUT_CARGO`（默认 cargo；自证用它换成假 cargo）可覆盖。
#
# ── 这个脚本存在的理由,以及它拦的假结论 ──────────────────────────
#
# 变异测试的产出是「测试有没有变红」。**每一种「没变红」都可能不是「判据不承重」**,
# 而是下面这些之一 —— 每一件都在实际的复审里发生过:
#
#   1. 锚点不存在 → 什么都没注入,而 `0 failed` 读起来和「没覆盖」一模一样。
#   2. 锚点不唯一 → 注入到了另一处(注释里那处),同样是「变异了但全绿」。
#   3. **纯插入**(替换里仍完整包含锚点)→ 原代码还在,行为没变。
#      **不改变行为的变异必然全绿,而那个绿和「没覆盖」分不开。**
#      (这条也会拒绝 `x → !(x)` 这种包裹式变异 —— 方向是保守的:报作废,不给假读数。
#       选一个替换里不会原样出现的锚点即可。)
#   4. **编译失败** → 拿到的是「编译不过」不是「测试红」。**编译错误不是测试结果。**
#   5. **测试数与基线不一致** → 过滤词打错(0 个测试)、或变异删掉/藏掉了一条测试
#      (`#[test]` → `#[allow(dead_code)]`:8 → 7 passed)。这时的「全绿」说的是
#      **另一组测试**,不是这条判据。所以基线必须 > 0 个测试,且每一格都与它逐数比对。
#      红基线同理:红基线上读出来的「红」不是这次变异造成的 —— 基线不绿就不许开始。
#
# 还拦两种非假结论但同样致命的:
#   - **挂住**。没有超时的话,「挂住」和「还在跑」是同一种表现 —— 一片安静。
#     超时杀的是**整个进程组**(cargo + 它起的测试进程),不只是直接子进程。
#   - **中途被杀**。`mut` 是原地改源码;被 INT/TERM 打断时先把源码恢复、再按原信号退出,
#     否则下一步很可能把 `if false {` 一起提交。
#
# 另一处判错的方向同样要防:测试进程**崩溃**(abort/段错误)时 cargo 也打 `error:`,
# 不能被读成「编译失败,作废」—— 变异让测试进程死掉,就是被抓到了,算 🔴。
#
# **仍然拦不住的**:被变异的文件在 crate 目录里、却不在模块树里(没有 `mod` 声明),
# `cargo test` 根本不编译它 → 假 🟢。选文件时自己确认它被编译。
#
# ── 一个试过并否定的做法,记在这里免得下一个人再试 ────────────────
#
# 曾想用「编译产物哈希是否变化」来自动识别第 3 类。**实测不成立**:加一个
# 没人用的 `const` 同样改变二进制哈希(基线 1324…、空操作 1bb3…、真变异 47c0…
# 三者互不相同)。哈希能证明「有变化」,证明不了「行为有变化」。改用第 3 条的
# 词法规则 —— 它拦不住所有空操作,但拦得住实际发生过的那一类,而且它不撒谎。

_MUT_CRATE=""; _MUT_FILTER=""; _MUT_BASE=""; _MUT_BASE_N=""; _MUT_CRATE_DIR=""
_MUT_LAST=""; _MUT_ABORTED=""

# macOS 没有 timeout(1),自己写一个。被测命令放进**自己的进程组**,超时时整组杀掉。
#   _mut_tmo <秒> <状态目录> <命令…>   → 超时返回 124,否则返回命令自己的退出码
_mut_tmo() {
  local secs=$1 st=$2; shift 2
  rm -f "$st/timed_out" "$st/done"
  perl -e 'setpgrp(0,0) or die "setpgrp: $!\n"; exec { $ARGV[0] } @ARGV or die "exec: $!\n"' "$@" &
  local p=$!
  echo "$p" >"$st/pgid"                       # setpgrp(0,0) → 进程组号 == pid
  # 先落标记再杀:这样 wait 一返回,标记就一定已经在了。
  # `sleep &&`:收尾时计时器的 sleep 是被杀掉的,被杀的 sleep 非 0 → 不会往下写「超时」。
  # (第一版用 `;`,于是每一次正常跑完都被读成了挂住 —— 自证的第一格就抓到了。)
  ( sleep "$secs" && [ ! -f "$st/done" ] && { : >"$st/timed_out"; kill -9 -- "-$p" 2>/dev/null; } ) &
  local k=$!
  wait "$p"; local rc=$?
  : >"$st/done"
  pkill -P "$k" 2>/dev/null; wait "$k" 2>/dev/null
  rm -f "$st/pgid" "$st/done"
  if [ -f "$st/timed_out" ]; then return 124; fi
  return "$rc"
}

# _mut_run <状态目录> → 打印一行:
#   "PASS <passed数> <result行>" / "FAIL <result行>" / "CRASH <说明>" / "COMPILE" / "TIMEOUT" / "UNKNOWN"
_mut_run() {
  local st=$1 out="$1/out" rc
  # shellcheck disable=SC2086 # _MUT_FILTER 为空时就不传参数,有意不加引号
  _mut_tmo "${MUT_TIMEOUT:-150}" "$st" "${MUT_CARGO:-cargo}" test -p "$_MUT_CRATE" --lib $_MUT_FILTER >"$out" 2>&1
  rc=$?
  if [ "$rc" -eq 124 ]; then echo "TIMEOUT"; return; fi
  local lines; lines=$(grep -E "^test result:" "$out")
  if [ -z "$lines" ]; then
    # 没有 result 行:只认得出两种,其余一律「不认识」—— 不认识不能落到 🔴 或 🟢。
    if grep -qE "^error\[E[0-9]+\]|could not compile" "$out"; then echo "COMPILE"
    elif grep -qE "process didn't exit successfully" "$out"; then
      echo "CRASH $(grep -m1 -E "process didn't exit successfully" "$out" | sed -E 's/.*\((.*)\)$/\1/')"
    else echo "UNKNOWN"; fi
    return
  fi
  local passed; passed=$(printf '%s\n' "$lines" | sed -nE 's/.* ([0-9]+) passed.*/\1/p' | awk '{s+=$1} END{print s+0}')
  if printf '%s\n' "$lines" | grep -qv "result: ok"; then
    echo "FAIL $(printf '%s\n' "$lines" | head -1)"
  elif [ "$rc" -ne 0 ]; then
    echo "UNKNOWN"                                # result 行全 ok,退出码却非 0 —— 不认识
  else
    echo "PASS $passed $(printf '%s\n' "$lines" | head -1)"
  fi
}

# 中断处理:先恢复源码(若有)、再杀那一组测试进程,然后按原信号退出。
_mut_abort() { # _mut_abort <信号> <源文件或空> <状态目录>
  local sig=$1 src=$2 st=$3
  if [ -n "$src" ] && [ -f "$st/bak" ]; then cp "$st/bak" "$src"; fi
  [ -f "$st/pgid" ] && kill -9 -- "-$(cat "$st/pgid")" 2>/dev/null
  rm -rf "$st"
  trap - INT TERM
  _MUT_ABORTED=1
  if [ -n "$src" ]; then printf "\n  ⛔ 被 %s 打断 —— 源码已恢复\n" "$sig" >&2
  else printf "\n  ⛔ 被 %s 打断\n" "$sig" >&2; fi
  kill -s "$sig" $$
}

# _mut_exec <状态目录> <源文件或空> → 结果写进 <状态目录>/res;被打断返回 130
# 放到后台再 wait:`wait` 会被已设 trap 的信号立刻打断,前台命令替换要等 cargo 跑完。
_mut_exec() {
  local st=$1 src=$2 rust
  rust=$(git rev-parse --show-toplevel)/rust
  _MUT_ABORTED=""
  # shellcheck disable=SC2064 # 路径在此刻展开,是有意的
  trap "_mut_abort INT $(printf %q "$src") $(printf %q "$st")" INT
  # shellcheck disable=SC2064
  trap "_mut_abort TERM $(printf %q "$src") $(printf %q "$st")" TERM
  ( cd "$rust" && _mut_run "$st" >"$st/res" ) &
  wait $!
  if [ -n "$_MUT_ABORTED" ]; then return 130; fi   # 交互式 shell 忽略 TERM,会走到这里
  trap - INT TERM
}

mut_baseline() { # mut_baseline <crate> [过滤]
  # 先清空:一次失败的重立基线,绝不能让上一次的好基线继续生效。
  _MUT_BASE=""; _MUT_BASE_N=""; _MUT_CRATE_DIR=""
  _MUT_CRATE=$1; _MUT_FILTER=${2:-}
  local root st res dir
  root=$(git rev-parse --show-toplevel) || return 1
  dir=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/rust/Cargo.toml" 2>/dev/null |
    CRATE="$_MUT_CRATE" python3 -c 'import json,os,sys
m=json.load(sys.stdin)
for p in m["packages"]:
    if p["name"]==os.environ["CRATE"]: print(os.path.dirname(p["manifest_path"]))' 2>/dev/null)
  [ -n "$dir" ] || { printf "  ⛔ 找不到 crate %s —— 停止\n" "$_MUT_CRATE"; return 1; }
  st=$(mktemp -d)
  _mut_exec "$st" "" || return 130
  res=$(cat "$st/res" 2>/dev/null); rm -rf "$st"
  case "$res" in
    PASS\ 0\ *) printf "  ⛔ 基线跑了 0 个测试(%s) —— 过滤词不对?停止\n" "${res#PASS 0 }"; return 1 ;;
    PASS*)
      _MUT_BASE=$res; _MUT_BASE_N=$(echo "$res" | awk '{print $2}'); _MUT_CRATE_DIR=${dir#"$root"/}
      printf "  %-44s %s\n" "基线(必须全绿,否则后面读数无意义)" "${res#PASS * }" ;;
    *) printf "  ⛔ 基线不是全绿(%s) —— 停止,先修基线\n" "$res"; return 1 ;;
  esac
}

mut() { # mut <文件> <锚点> <替换> <标签>
  local file=$1 anchor=$2 repl=$3 label=$4
  _MUT_LAST=""
  case "$_MUT_BASE" in PASS*) ;; *) echo "  ⛔ 没有可用的基线(先 mut_baseline,且必须全绿),拒绝开始"; return 1 ;; esac
  local root; root=$(git rev-parse --show-toplevel) || return 1
  local src="$root/$file"
  [ -f "$src" ] || { printf "  ⛔ 文件不存在: %s\n" "$file"; return 1; }
  case "$file" in
    "$_MUT_CRATE_DIR"/*) ;;
    *) printf "  ⛔ %s 不在基线 crate(%s)里 —— cargo test -p 不会编译它,读数会是假 🟢\n" "$file" "$_MUT_CRATE_DIR"; return 1 ;;
  esac
  local st; st=$(mktemp -d)
  cp "$src" "$st/bak" || { rm -rf "$st"; echo "  ⛔ 备份失败,不动源码"; return 1; }

  local msg rc
  msg=$(ANCHOR="$anchor" REPL="$repl" python3 - "$src" <<'PY'
import io,os,sys
p=sys.argv[1]; a=os.environ["ANCHOR"]; b=os.environ["REPL"]
s=io.open(p,encoding='utf-8').read()
if a not in s:            print("锚点不存在"); sys.exit(1)
if s.count(a)>1:          print(f"锚点出现 {s.count(a)} 次,不唯一"); sys.exit(1)
if a==b:                  print("替换与锚点相同"); sys.exit(1)
if a in b:                print("纯插入:替换里仍完整包含锚点,原代码还在 → 行为未必变"); sys.exit(1)
io.open(p,'w',encoding='utf-8').write(s.replace(a,b,1))
PY
)
  rc=$?
  if [ $rc -ne 0 ]; then
    cp "$st/bak" "$src"; rm -rf "$st"
    _MUT_LAST=VOID
    printf "  %-44s ⛔ 注入自证失败(%s),不读测试结果\n" "$label" "$msg"; return 0
  fi

  _mut_exec "$st" "$src" || return 130
  local r; r=$(cat "$st/res" 2>/dev/null)
  cp "$st/bak" "$src"; rm -rf "$st"

  case "$r" in
    FAIL*)    _MUT_LAST=RED;   printf "  %-44s 🔴 %s\n" "$label" "${r#FAIL }" ;;
    CRASH*)   _MUT_LAST=RED;   printf "  %-44s 🔴 测试进程崩溃(%s)\n" "$label" "${r#CRASH }" ;;
    PASS*)
      local n; n=$(echo "$r" | awk '{print $2}')
      if [ "$n" != "$_MUT_BASE_N" ]; then
        _MUT_LAST=VOID; printf "  %-44s ⛔ 测试数与基线不一致(%s ≠ 基线 %s),本格作废\n" "$label" "$n" "$_MUT_BASE_N"
      else
        _MUT_LAST=ALIVE; printf "  %-44s 🟢 存活(判据不承重) %s\n" "$label" "${r#PASS * }"
      fi ;;
    COMPILE)  _MUT_LAST=VOID;    printf "  %-44s ⛔ 编译失败 —— 不是测试红,本格作废\n" "$label" ;;
    TIMEOUT)  _MUT_LAST=TIMEOUT; printf "  %-44s ⏱ 挂住 —— 不是测试结果,去看它挂在哪\n" "$label" ;;
    *)        _MUT_LAST=VOID;    printf "  %-44s ⛔ 无法识别的输出,本格作废\n" "$label" ;;
  esac
}
