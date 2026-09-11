#!/usr/bin/env bash
# `mutate.sh` / `mutate.py` 的自证 —— 一个拦不住假结论的脚手架，会安静地把假结论当成读数。
#
#   bash docs/agent/mutate-selftest.sh        # 全部通过 exit 0,任何一格不符 exit 1
#
# 每一格都**断言**判定。三类对照缺一不可:🔴(⑤⑦⑮)、🟢(⑧)、⛔ —— 只有前两类中的任意一类,
# 一个「永远报那一类」的脚手架就能通过其余每一格。(上一版没有 🟢 对照,复审实测:把 PASS 格
# 全改成作废,自证照样全过。)
#
# 真 cargo 的格子验「对真实输出判得对不对」;假 cargo(`MUT_CARGO`)的格子验真 cargo 造不出来
# 或要等很久的形态:红基线、矛盾输出、挂住带孙进程、在每个相位上被信号打断、并发、恢复失败。
# 进程是否死干净按**假 cargo 自己记下的 pid** 逐个核对,不按命令行模式去猜。
set -u
cd "$(dirname "$0")/../.." || exit 1
ROOT=$(pwd)
GITDIR=$(git rev-parse --absolute-git-dir)
# shellcheck source=docs/agent/mutate.sh
source docs/agent/mutate.sh

CRATE_DIR=rust/crates/agent24-os-proto
F=$CRATE_DIR/src/supervise.rs
FA=$ROOT/$F
BREAKER='        if self.consecutive >= BREAKER_THRESHOLD {'
ORPHAN=$CRATE_DIR/src/zz_mutate_selftest_orphan.rs
ORIG=$(mktemp)
cp "$F" "$ORIG"
FAKE=$(mktemp -d)
cleanup() {
  chmod u+w "$CRATE_DIR/src" 2>/dev/null
  [ -d "$GITDIR/mutate-inflight" ] && python3 docs/agent/mutate.py recover >/dev/null
  cp "$ORIG" "$F"
  rm -f "$ORPHAN" "$GITDIR"/mutate-paused-* "$GITDIR"/mutate-continue-*
  [ -f "$FAKE/pids" ] && xargs kill -9 <"$FAKE/pids" 2>/dev/null
  rm -rf "$ORIG" "$FAKE"
}
trap cleanup EXIT
bad=0
expect() { # expect <期望> <实际> <格名>
  if [ "$1" = "$2" ]; then printf "    ✓ %s\n" "$3"; else
    printf "    ✗ %s:期望 %s,实际 %s\n" "$3" "$1" "${2:-<空>}"
    bad=1
  fi
}
same_as_orig() { cmp -s "$F" "$ORIG" && echo same || echo changed; }
journal() { [ -d "$GITDIR/mutate-inflight" ] && echo present || echo none; }
all_dead() { # 假 cargo 记下的每一个 pid 都不在了
  local p
  [ -s "$FAKE/pids" ] || { echo "no-pids-recorded"; return; }
  while read -r p; do kill -0 "$p" 2>/dev/null && { echo "alive:$p"; return; }; done <"$FAKE/pids"
  echo dead
}
wait_for() { # wait_for <文件> —— 等它出现,最多 30s
  local i
  for i in $(seq 1 1500); do [ -e "$1" ] && return 0; sleep 0.02; done
  return 1
}
fake() { # fake <名字> <脚本体>:每个假 cargo 都把自己的 pid 记进 $FAKE/pids
  printf '#!/usr/bin/env bash\necho $$ >>%q\n%s\n' "$FAKE/pids" "$2" >"$FAKE/$1"
  chmod +x "$FAKE/$1"
}
SUM_OK='echo "test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"'
SUM_RED='echo "test result: FAILED. 7 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"'
# cond:只有当源码被变异成 `if false {` 时才红 —— 让「红是变异造成的」这件事可控。
fake ok "$SUM_OK"
fake red "$SUM_RED; exit 101"
fake cond "if grep -q 'if false {' $(printf %q "$FA"); then $SUM_RED; exit 101; else $SUM_OK; fi"
fake cond124 "if grep -q 'if false {' $(printf %q "$FA"); then $SUM_RED; exit 124; else $SUM_OK; fi"
fake weird 'echo "something unexpected"; exit 1'
fake twosum "$SUM_OK; $SUM_OK"
fake buildrs 'echo "error: failed to run custom build command for \`x v0.1.0\`"; echo "  process didn'"'"'t exit successfully: \`/t/build-script-build\` (exit status: 101)"; exit 101'
fake hang "sleep 3001 & echo \$! >>$(printf %q "$FAKE/pids"); sleep 3002 & echo \$! >>$(printf %q "$FAKE/pids"); wait"
fake slow "sleep 3003 & echo \$! >>$(printf %q "$FAKE/pids"); : >$(printf %q "$FAKE/running"); wait"

echo "── 真 cargo ──"
mut_baseline agent24-os-proto supervise || { echo "  基线立不起来,自证无法进行"; exit 1; }

mut "$F" 'NO-SUCH-ANCHOR' 'x' "① 锚点不存在";          expect VOID "$_MUT_LAST" "① → 作废"
mut "$F" '    }' '    };' "② 锚点不唯一";               expect VOID "$_MUT_LAST" "② → 作废"
mut "$F" 'pub const REAP_TIMEOUT' 'const UNUSED_XYZ: u32 = 7;
pub const REAP_TIMEOUT' "③ 纯插入(空操作)";              expect VOID "$_MUT_LAST" "③ → 作废"
mut "$F" "$BREAKER" '        if self.consecutive >= BREAKER_THRESHOLD (' "④ 编译失败"
expect VOID "$_MUT_LAST" "④ → 作废"
mut "$F" "$BREAKER" '        if false {' "⑤ 对照:真变异";   expect RED "$_MUT_LAST" "⑤ → 必须红"
mut "$F" '#[test]
    fn only_a_long_enough_run_counts_as_healthy' '#[allow(dead_code)]
    fn only_a_long_enough_run_counts_as_healthy' "⑥ 藏掉一条测试"
expect VOID "$_MUT_LAST" "⑥ 测试数少了一条 → 作废,不是 🟢"
mut "$F" "$BREAKER" '        if { std::process::abort() } {' "⑦ 测试进程崩溃"
expect RED "$_MUT_LAST" "⑦ 崩溃 → 红,不是「编译失败」"
mut "$F" 'pub const REAP_TIMEOUT: Duration = Duration::from_secs(5);' \
  'pub const REAP_TIMEOUT: Duration = Duration::from_secs(6);' "⑧ 对照:没有测试管的常量"
expect ALIVE "$_MUT_LAST" "⑧ → 必须 🟢(否则一个永远报不出 🟢 的脚手架也能过)"
mut "$F" '        // 500ms, 1s, 2s, 4s' '        // 1s, 2s, 4s, 8s' "⑨ 锚点在注释里"
expect VOID "$_MUT_LAST" "⑨ → 作废"

mut "$CRATE_DIR/src/supervize_TYPO.rs" 'a' 'b' "⑩" >/dev/null
expect 1 $? "⑩ 路径打错 → 拒绝"
expect no "$([ -e "$CRATE_DIR/src/supervize_TYPO.rs" ] && echo yes || echo no)" "⑩ 且没有凭空建出文件"
mut rust/crates/agent24-domain/src/lib.rs 'a' 'b' "⑪" >/dev/null
expect 1 $? "⑪ 另一个 crate 的文件 → 拒绝"
mut "$CRATE_DIR/../agent24-domain/src/lib.rs" 'a' 'b' "⑪b" >/dev/null
expect 1 $? "⑪ 用 ../ 绕 → 同样拒绝"
printf 'pub fn orphan() -> bool { true }\n' >"$ORPHAN"
mut "$ORPHAN" 'true' 'false' "⑫" >/dev/null
expect 1 $? "⑫ 在 crate 目录里但不在模块树里 → 拒绝(不是假 🟢)"
rm -f "$ORPHAN"

mut_baseline agent24-os-proto nosuch_filter_typo_xyz >/dev/null
expect 1 $? "⑬ 0 个测试的基线 → 拒绝"
mut "$F" "$BREAKER" '        if false {' "⑬" >/dev/null
expect 1 $? "⑬ 且之前那条好基线不再生效"

echo "── 假 cargo:判读 ──"
MUT_CARGO=$FAKE/red mut_baseline agent24-os-proto >/dev/null
expect 1 $? "⑭ 红基线 → 拒绝"
mut "$F" "$BREAKER" '        if false {' "⑭" >/dev/null
expect 1 $? "⑭ 且 mut 拒绝开始"

MUT_CARGO=$FAKE/cond mut_baseline agent24-os-proto >/dev/null || { echo "  假基线立不起来"; exit 1; }
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "⑮ 红,且未变异时复跑是绿"
expect RED "$_MUT_LAST" "⑮ → 红"
MUT_CARGO=$FAKE/red mut "$F" "$BREAKER" '        if false {' "⑯ 未变异时也红(基线漂移)"
expect VOID "$_MUT_LAST" "⑯ → 作废,不是 🔴"
MUT_CARGO=$FAKE/weird mut "$F" "$BREAKER" '        if false {' "⑰ 不认识的输出"
expect VOID "$_MUT_LAST" "⑰ → 作废"
MUT_CARGO=$FAKE/twosum mut "$F" "$BREAKER" '        if false {' "⑱ 两行 result"
expect VOID "$_MUT_LAST" "⑱ → 作废(两个 4 也能凑成 8)"
MUT_CARGO=$FAKE/buildrs mut "$F" "$BREAKER" '        if false {' "⑲ build script 失败"
expect VOID "$_MUT_LAST" "⑲ → 作废,不是「测试进程崩溃」"
MUT_CARGO=$FAKE/cond124 mut "$F" "$BREAKER" '        if false {' "⑳ 自然退出 124"
expect RED "$_MUT_LAST" "⑳ → 按输出判红,不是「挂住」"

: >"$FAKE/pids"
MUT_TIMEOUT=2 MUT_CARGO=$FAKE/hang mut "$F" "$BREAKER" '        if false {' "㉑ 挂住(带孙进程)"
expect TIMEOUT "$_MUT_LAST" "㉑ → 挂住"
expect dead "$(all_dead)" "㉑ 且 cargo 与它的孙进程都已被杀"
expect same "$(same_as_orig)" "㉑ 且源码已恢复"

echo "── 假 cargo:在每个相位上被打断(非交互) ──"
# 不能写成 pid=$(run_bg …):命令替换是子 shell,后台进程成了它的孩子,主 shell 的 wait
# 等不到(返回 127),断言就会在 mut 收尾之前跑。所以 pid 放进全局变量。
run_bg() { # run_bg <MUT_CARGO> <MUT_PAUSE_AT 或空>:在独立 bash 里跑一格,pid 放进 $BG
  bash -c "cd $(printf %q "$ROOT") && source docs/agent/mutate.sh \
    && MUT_CARGO=$(printf %q "$1") MUT_PAUSE_AT=$(printf %q "$2") \
       mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' x" >/dev/null 2>&1 &
  BG=$!
}
# INT/QUIT 不在这里测:非交互 shell 起的后台命令对它们是 SIG_IGN 继承来的,trap 不了 ——
# Ctrl-C 的场景由下面的交互式格子测。
for cell in "after-journal TERM 143" "after-inject HUP 129" "child TERM 143" "after-run HUP 129"; do
  # shellcheck disable=SC2086
  set -- $cell
  phase=$1 sig=$2 want=$3
  : >"$FAKE/pids"
  rm -f "$FAKE/running"
  if [ "$phase" = child ]; then
    run_bg "$FAKE/slow" ""
    pid=$BG
    wait_for "$FAKE/running" || echo "    (假 cargo 没起来)"
    expect changed "$(same_as_orig)" "㉒ $phase 前提:此刻源码处于被变异状态"
  else
    run_bg "$FAKE/cond" "$phase"
    pid=$BG
    wait_for "$GITDIR/mutate-paused-$phase" || echo "    (没停在 $phase)"
  fi
  kill -s "$sig" "$pid"
  { wait "$pid"; } 2>/dev/null
  rc=$?
  expect "$want" "$rc" "㉒ $phase 收到 SIG$sig → 以 $want 退出"
  expect same "$(same_as_orig)" "㉒ $phase 源码已恢复"
  expect none "$(journal)" "㉒ $phase 日志已清"
  [ "$phase" = child ] && expect dead "$(all_dead)" "㉒ $phase 测试进程组已被杀"
done

echo "── 交互式 shell 里 Ctrl-C ──"
: >"$FAKE/pids"
rm -f "$FAKE/running"
PTY_OUT=$(ROOT="$ROOT" F="$F" BREAKER="$BREAKER" SLOW="$FAKE/slow" RUNNING="$FAKE/running" python3 - <<'PY'
import os, pty, select, time, shlex
pid, fd = pty.fork()
if pid == 0:
    os.execvp("bash", ["bash", "--norc", "--noprofile", "-i"])
out = b""
def send(s): os.write(fd, s.encode())
def drain(t):
    global out
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: out += os.read(fd, 4096)
            except OSError: return
send(f"cd {shlex.quote(os.environ['ROOT'])} && source docs/agent/mutate.sh\n")
send(f"MUT_CARGO={shlex.quote(os.environ['SLOW'])} mut {shlex.quote(os.environ['F'])} "
     f"{shlex.quote(os.environ['BREAKER'])} '        if false {{' x\n")
end = time.time() + 30
while not os.path.exists(os.environ["RUNNING"]) and time.time() < end:
    drain(0.1)
send("\x03")
drain(2)
send("echo SURVIVED:$?\n")
drain(2)
send("exit\n")
drain(1)
print("SURVIVED:130" in out.decode(errors="replace"))
PY
)
expect True "$PTY_OUT" "㉓ Ctrl-C 之后交互式 shell 还在,且 \$?=130"
expect same "$(same_as_orig)" "㉓ 源码已恢复"
expect dead "$(all_dead)" "㉓ 测试进程组已被杀"

echo "── 并发与恢复失败 ──"
run_bg "$FAKE/cond" after-inject
pidA=$BG
wait_for "$GITDIR/mutate-paused-after-inject"
out=$(MUT_CARGO=$FAKE/cond mut "$F" 'pub const REAP_TIMEOUT' 'pub(crate) const REAP_TIMEOUT' "㉔")
expect 1 $? "㉔ 另一个 mut 正在跑 → 拒绝(不会把它的变异当原文备份)"
# 拒绝的理由必须是「正在跑」而不是「上次没收尾,去 recover」:后者会让人在别人跑到一半
# 时把文件写回去。(元测试实测:去掉锁,单靠日志目录也能拒 —— 但给的是后一种理由。)
expect yes "$([[ $out == *正在跑* ]] && echo yes || echo no)" "㉔ 且理由是「正在跑」,不是叫人去 recover"
mut_recover >/dev/null
expect 1 $? "㉔ 别人跑到一半时 recover 也被拒"
expect changed "$(same_as_orig)" "㉔ 且没有把它的变异提前写回去"
: >"$GITDIR/mutate-continue-after-inject"
{ wait "$pidA"; } 2>/dev/null
expect same "$(same_as_orig)" "㉔ 第一个跑完后源码是原样"

run_bg "$FAKE/cond" after-run
pidA=$BG
wait_for "$GITDIR/mutate-paused-after-run"
chmod a-w "$CRATE_DIR/src"
: >"$GITDIR/mutate-continue-after-run"
{ wait "$pidA"; } 2>/dev/null
rc=$?
chmod u+w "$CRATE_DIR/src"
expect 2 "$rc" "㉕ 恢复不了 → 返回 2,不报读数"
expect present "$(journal)" "㉕ 且保留备份(不删唯一的一份)"
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉕" >/dev/null
expect 1 $? "㉕ 有未收尾的变异时拒绝开始"
mut_recover >/dev/null
expect 0 $? "㉕ recover 成功"
expect same "$(same_as_orig)" "㉕ recover 之后源码是原样"
expect none "$(journal)" "㉕ 且日志已清"

got=$(bash -c "cd $(printf %q "$ROOT") && trap 'echo MINE' TERM && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/ok") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' x >/dev/null; trap -p TERM")
expect yes "$([[ $got == *MINE* ]] && echo yes || echo no)" "㉖ 调用方原有的 trap 在 mut 之后还在"

echo
expect same "$(same_as_orig)" "收尾:被变异的文件与开始时逐字节相同"
expect none "$(journal)" "收尾:没有未收尾的日志"
if [ "$bad" -ne 0 ]; then
  echo "⛔ 自证失败 —— 脚手架本身坏了,这一轮所有变异读数作废(而不是去调这个自证)"
  exit 1
fi
echo "✓ 自证全部通过"
