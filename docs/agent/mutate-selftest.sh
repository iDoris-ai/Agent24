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
FRAME=$CRATE_DIR/src/frame.rs
FRAME_ORIG=$(mktemp)
cp "$FRAME" "$FRAME_ORIG"
cleanup() {
  chmod u+w "$F" "$CRATE_DIR/src" 2>/dev/null
  cp "$FRAME_ORIG" "$FRAME"
  rm -f "$FRAME_ORIG"
  [ -d "$GITDIR/mutate-inflight" ] && python3 docs/agent/mutate.py recover >/dev/null
  cp "$ORIG" "$F"
  rm -f "$ORPHAN" "$GITDIR"/mutate-paused-* "$GITDIR"/mutate-continue-*
  [ -f "$FAKE/allpids" ] && xargs kill -9 <"$FAKE/allpids" 2>/dev/null
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
fake() { # fake <名字> <脚本体>:每个假 cargo 都把自己的 pid 记进 $FAKE/pids(本格)与 allpids(收尾用)
  printf '#!/usr/bin/env bash\necho $$ >>%q; echo $$ >>%q\n%s\n' "$FAKE/pids" "$FAKE/allpids" "$2" >"$FAKE/$1"
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
fake hang "sleep 3001 & echo \$! | tee -a $(printf %q "$FAKE/pids") $(printf %q "$FAKE/allpids") >/dev/null; sleep 3002 & echo \$! | tee -a $(printf %q "$FAKE/pids") $(printf %q "$FAKE/allpids") >/dev/null; wait"
fake slow "sleep 3003 & echo \$! | tee -a $(printf %q "$FAKE/pids") $(printf %q "$FAKE/allpids") >/dev/null; : >$(printf %q "$FAKE/running"); wait"
PIDS=$(printf %q "$FAKE/pids")
fake okrc1 "$SUM_OK; exit 1"
fake failrc0 "$SUM_RED; exit 0"
MUTATED="grep -q 'if false {' $(printf %q "$FA")"
fake foreign_ 'echo "     Running unittests src/lib.rs (target/debug/deps/x-1)"; echo "error: test failed"; echo "  process didn'"'"'t exit successfully: \`/r/target/debug/deps/other-2\` (signal: 6, SIGABRT)"; exit 101'
fake prefix_ 'echo "     Running unittests src/lib.rs (target/debug/deps/x-1)"; echo "error: test failed"; echo "  process didn'"'"'t exit successfully: \`/r/target/debug/deps/x-12345\` (signal: 6, SIGABRT)"; exit 101'
# ㉘ 的两个只在变异时崩:否则「未变异复跑」那道确认会从下游把它们兜成作废,分类规则本身
# 就没被测到(元测试实测:去掉「只认那个二进制」,自证照过)。
fake foreign "if $MUTATED; then $FAKE/foreign_; exit 101; else $SUM_OK; fi"
fake prefix "if $MUTATED; then $FAKE/prefix_; exit 101; else $SUM_OK; fi"
fake indent "echo '    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; $SUM_OK"
fake leak "sleep 3005 & echo \$! | tee -a $PIDS $(printf %q "$FAKE/allpids") >/dev/null; $SUM_OK"
fake detach "perl -e 'setpgrp(0,0); sleep 3006' & echo \$! | tee -a $PIDS $(printf %q "$FAKE/allpids") >/dev/null; sleep 3007 & echo \$! | tee -a $PIDS $(printf %q "$FAKE/allpids") >/dev/null; wait"
# cond7:被变异时红;未变异时,$FAKE/drift 存在就只报 7 个通过(基线是 8)。
fake cond7 "if grep -q 'if false {' $(printf %q "$FA"); then $SUM_RED; exit 101; elif [ -e $(printf %q "$FAKE/drift") ]; then echo 'test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; else $SUM_OK; fi"
# flaky:被变异时第一次红、第二次绿 —— 偶发失败
fake flaky "n=\$(cat $(printf %q "$FAKE/cnt") 2>/dev/null || echo 0); echo \$((n+1)) >$(printf %q "$FAKE/cnt"); if grep -q 'if false {' $(printf %q "$FA") && [ \"\$n\" = 0 ]; then $SUM_RED; exit 101; else $SUM_OK; fi"
# failed0:变异时报 FAILED 却 0 failed —— 自相矛盾,不能读成红
fake failed0 "if $MUTATED; then echo 'test result: FAILED. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; exit 101; else $SUM_OK; fi"
# tworun:变异时两行 Running、一行崩溃 —— 认不出崩的是哪个
# 崩溃行里用**基线记下的那个真实二进制路径**:否则路径匹配那一关先就不认,「Running 行数」
# 这一关根本走不到(元测试实测:放宽成只看路径,这格照过)。
fake tworun_ "EXE=\$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[\"exe\"])' $(printf %q "$GITDIR/mutate-baseline.json")); echo \"     Running unittests src/lib.rs (\$EXE)\"; echo '     Running unittests src/lib.rs (target/debug/deps/y-2)'; echo 'error: test failed'; echo \"  process didn't exit successfully: \\\`\$EXE\\\` (signal: 6, SIGABRT)\"; exit 101"
fake tworun "if $MUTATED; then $FAKE/tworun_; exit 101; else $SUM_OK; fi"
# names:变异时两次都红,但失败的测试不是同一批
fake names "n=\$(cat $(printf %q "$FAKE/cnt") 2>/dev/null || echo 0); echo \$((n+1)) >$(printf %q "$FAKE/cnt"); if $MUTATED; then echo \"test t\$n ... FAILED\"; $SUM_RED; exit 101; else $SUM_OK; fi"
fake slowmeta "sleep 3008 & echo \$! | tee -a $PIDS $(printf %q "$FAKE/allpids") >/dev/null; : >$(printf %q "$FAKE/running"); wait"

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
# 在 CARGO_TERM_COLOR=always 下跑:颜色转义会让行首锚定的判据全部失配。
CARGO_TERM_COLOR=always mut "$F" "$BREAKER" '        if { std::process::abort() } {' "⑦ 测试进程崩溃(彩色输出下)"
expect RED "$_MUT_LAST" "⑦ 崩溃 → 红,不是「编译失败」或「不认识」"
# cargo -q 不打印 `Running unittests` 行;崩溃要靠基线记下的二进制路径来认。
mut_baseline agent24-os-proto supervise -q >/dev/null || echo "    (-q 基线立不起来)"
mut "$F" "$BREAKER" '        if { std::process::abort() } {' "⑦b 崩溃(cargo -q)" >/dev/null
expect RED "$_MUT_LAST" "⑦b -q 下的崩溃仍然是红"
mut_baseline agent24-os-proto supervise >/dev/null || { echo "  基线立不起来"; exit 1; }
mut "$F" 'pub const REAP_TIMEOUT: Duration = Duration::from_secs(5);' \
  'pub const REAP_TIMEOUT: Duration = Duration::from_secs(6);' "⑧ 对照:没有测试管的常量"
expect ALIVE "$_MUT_LAST" "⑧ → 必须 🟢(否则一个永远报不出 🟢 的脚手架也能过)"
mut "$F" '        // 500ms, 1s, 2s, 4s' '        // 1s, 2s, 4s, 8s' "⑨ 锚点在注释里"
expect VOID "$_MUT_LAST" "⑨ → 作废"
# ⑨b 块注释里的改动同样作废(复审 @ #172 F2)。用临时加进去的一段块注释当靶子。
python3 - "$F" <<'PY'
import sys; p=sys.argv[1]; s=open(p).read()
open(p,"w").write(s.replace("pub const REAP_TIMEOUT", "/* block note: retries 3 */\npub const REAP_TIMEOUT", 1))
PY
mut_baseline agent24-os-proto supervise >/dev/null || echo "    (带块注释的基线立不起来)"
mut "$F" 'retries 3' 'retries 4' "⑨b 锚点在块注释里"
expect VOID "$_MUT_LAST" "⑨b → 作废"
cp "$ORIG" "$F"
mut_baseline agent24-os-proto supervise >/dev/null || { echo "  基线立不起来"; exit 1; }

mut "$CRATE_DIR/src/supervize_TYPO.rs" 'a' 'b' "⑩" >/dev/null
expect 1 $? "⑩ 路径打错 → 拒绝"
expect no "$([ -e "$CRATE_DIR/src/supervize_TYPO.rs" ] && echo yes || echo no)" "⑩ 且没有凭空建出文件"
mut rust/crates/agent24-domain/src/lib.rs 'a' 'b' "⑪" >/dev/null
expect 1 $? "⑪ 另一个 crate 的文件 → 拒绝"
mut "$CRATE_DIR/../agent24-domain/src/lib.rs" 'a' 'b' "⑪b" >/dev/null
expect 1 $? "⑪ 用 ../ 绕 → 同样拒绝"
# 先建孤儿、再立基线:指纹覆盖 rust/ 下全部文件,基线之后才建的话,拒绝理由会变成
# 「树变了」—— 这格就不再测「不在编译范围」了。
printf 'pub fn orphan() -> bool { true }\n' >"$ORPHAN"
mut_baseline agent24-os-proto supervise >/dev/null || echo "    (带孤儿文件的基线立不起来)"
out=$(mut "$ORPHAN" 'true' 'false' "⑫")
expect 1 $? "⑫ 在 crate 目录里但不在模块树里 → 拒绝(不是假 🟢)"
expect yes "$([[ $out == *编译范围* ]] && echo yes || echo no)" "⑫ 且理由是「不在编译范围」"
rm -f "$ORPHAN"
mut_baseline agent24-os-proto supervise >/dev/null || { echo "  基线立不起来"; exit 1; }

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
for cell in "after-journal TERM 143" "after-inject HUP 129" "child TERM 143" "after-run HUP 129" "before-verdict TERM 143"; do
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
    if [ "$phase" = after-journal ]; then
      # 顺序:备份先落盘、再动源码。停在这里时源码必须仍是原文,且日志已完整在位。
      expect same "$(same_as_orig)" "㉒ after-journal 前提:源码还没被动"
      expect same "$(cmp -s "$GITDIR/mutate-inflight/bak" "$ORIG" && echo same || echo changed)" "㉒ after-journal 前提:备份已完整落盘"
    fi
  fi
  kill -s "$sig" "$pid"
  { wait "$pid"; } 2>/dev/null
  rc=$?
  expect "$want" "$rc" "㉒ $phase 收到 SIG$sig → 以 $want 退出"
  expect same "$(same_as_orig)" "㉒ $phase 源码已恢复"
  expect none "$(journal)" "㉒ $phase 日志已清"
  [ "$phase" = child ] && expect dead "$(all_dead)" "㉒ $phase 测试进程组已被杀"
done

# ㉒b 同一件事,绕过 bash 包装直接对 python 发信号:包装会在 shell 这一层把信号重新交还
# 一遍,所以上面几格即使 python 返回了一个普通读数,脚本照样以 143 退出 —— python 自己的
# 退出码没被测到(元测试实测:两道防线同时拿掉,上面全过)。
MUT_CARGO=$FAKE/cond MUT_PAUSE_AT=before-verdict python3 docs/agent/mutate.py mut "$F" "$BREAKER" '        if false {' x >/dev/null 2>&1 &
pypid=$!
wait_for "$GITDIR/mutate-paused-before-verdict"
kill -TERM "$pypid"; { wait "$pypid"; } 2>/dev/null
expect 143 $? "㉒b 读数定下之前收到的 TERM,python 自己也以 143 退出,不报读数"
expect same "$(same_as_orig)" "㉒b 源码已恢复"

# ㉒c 非交互脚本收到 INT:整批停下,第二格从未开始,脚本以 130 退出。(复审 @ #172 F1:bash 的
# wait-and-cooperative-exit —— 用子进程把 INT 交还给 shell 时,那个子进程是正常退出的,bash 就
# 不退出,继续跑下一格。)脚本用 perl 恢复 INT 的默认处置再启动:否则后台作业继承 SIG_IGN,
# 这个实验本身就无效。
: >"$FAKE/pids"; rm -f "$FAKE/running" "$FAKE/cell2"
perl -e '$SIG{INT}="DEFAULT"; exec @ARGV' bash -c "cd $(printf %q "$ROOT") && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/slow") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' c1; \
  : >$(printf %q "$FAKE/cell2"); MUT_CARGO=$(printf %q "$FAKE/ok") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' c2" >/dev/null 2>&1 &
spid=$!
wait_for "$FAKE/running"
kill -INT "$spid"; { wait "$spid"; } 2>/dev/null
expect 130 $? "㉒c 非交互脚本收到 INT → 以 130 退出"
expect no "$([ -e "$FAKE/cell2" ] && echo yes || echo no)" "㉒c 且第二格从未开始"
expect same "$(same_as_orig)" "㉒c 源码已恢复"

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
chmod a-w "$F"
: >"$GITDIR/mutate-continue-after-run"
{ wait "$pidA"; } 2>/dev/null
rc=$?
chmod u+w "$F"
expect 2 "$rc" "㉕ 恢复不了 → 返回 2,不报读数"
expect present "$(journal)" "㉕ 且保留备份(不删唯一的一份)"
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉕" >/dev/null
expect 1 $? "㉕ 有未收尾的变异时拒绝开始"
# 用永远全绿的 ok:用 cond 的话,源码此刻是变异状态,基线本来就会因「不是全绿」被拒 ——
# 这格就不再测「有日志时拒绝」了(元测试实测:去掉那道检查,这格照过)。
MUT_CARGO=$FAKE/ok mut_baseline agent24-os-proto >/dev/null
expect 1 $? "㉕ 有未收尾的变异时也不许立基线(否则基线立在被变异的源码上)"
mut_recover >/dev/null
expect 0 $? "㉕ recover 成功"
expect same "$(same_as_orig)" "㉕ recover 之后源码是原样"
expect none "$(journal)" "㉕ 且日志已清"

got=$(bash -c "cd $(printf %q "$ROOT") && trap 'echo MINE' TERM && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/ok") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' x >/dev/null; trap -p TERM")
expect yes "$([[ $got == *MINE* ]] && echo yes || echo no)" "㉖ 调用方原有的 trap 在 mut 之后还在"

echo "── 复审 @ 46f67d3 补的格子 ──"
MUT_CARGO=$FAKE/cond mut_baseline agent24-os-proto >/dev/null || { echo "  假基线立不起来"; exit 1; }
MUT_CARGO=$FAKE/okrc1 mut "$F" "$BREAKER" '        if false {' "㉗ ok 行但退出码非 0" >/dev/null
expect VOID "$_MUT_LAST" "㉗ result ok 而 rc≠0 → 作废"
MUT_CARGO=$FAKE/failrc0 mut "$F" "$BREAKER" '        if false {' "㉗b" >/dev/null
expect VOID "$_MUT_LAST" "㉗ result FAILED 而 rc=0 → 作废"
MUT_CARGO=$FAKE/foreign mut "$F" "$BREAKER" '        if false {' "㉘" >/dev/null
expect VOID "$_MUT_LAST" "㉘ 崩的是另一个进程 → 作废,不是 🔴"
MUT_CARGO=$FAKE/prefix mut "$F" "$BREAKER" '        if false {' "㉘b" >/dev/null
expect VOID "$_MUT_LAST" "㉘ x-1 不认下 x-12345"
MUT_CARGO=$FAKE/indent mut "$F" "$BREAKER" '        if false {' "㉙" >/dev/null
expect ALIVE "$_MUT_LAST" "㉙ 测试自己打印的缩进 result 行不算数(行首锚定)"
: >"$FAKE/pids"
MUT_CARGO=$FAKE/leak mut "$F" "$BREAKER" '        if false {' "㉚" >/dev/null
expect dead "$(all_dead)" "㉚ 正常退出时 cargo 留下的后台进程也被杀"
: >"$FAKE/pids"
MUT_TIMEOUT=2 MUT_CARGO=$FAKE/detach mut "$F" "$BREAKER" '        if false {' "㉚b" >/dev/null
expect TIMEOUT "$_MUT_LAST" "㉚ 挂住"
expect dead "$(all_dead)" "㉚ 另起进程组的后代也被杀"
rm -f "$FAKE/drift"
MUT_CARGO=$FAKE/cond7 mut_baseline agent24-os-proto >/dev/null
: >"$FAKE/drift"
MUT_CARGO=$FAKE/cond7 mut "$F" "$BREAKER" '        if false {' "㉛" >/dev/null
expect VOID "$_MUT_LAST" "㉛ 未变异复跑是绿但测试数不同 → 作废"
rm -f "$FAKE/drift"
MUT_CARGO=$FAKE/cond mut_baseline agent24-os-proto >/dev/null
echo 0 >"$FAKE/cnt"
MUT_CARGO=$FAKE/flaky mut "$F" "$BREAKER" '        if false {' "㉜" >/dev/null
expect VOID "$_MUT_LAST" "㉜ 变异版第一次红、第二次绿(偶发)→ 作废,不是 🔴"
MUT_CARGO=$FAKE/cond mut "$FRAME" '// no newline, then EOF' '// no newline, then eof' "㉝" >/dev/null
expect VOID "$_MUT_LAST" "㉝ 改动落在行尾注释里 → 作废"
printf '\n// drift\n' >>"$FRAME"
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉞" >/dev/null
expect 1 $? "㉞ 基线之后别的源码变了 → 拒绝开始"
cp "$FRAME_ORIG" "$FRAME"
DOMAIN=rust/crates/agent24-domain/src/lib.rs; DOMAIN_ORIG=$(mktemp); cp "$DOMAIN" "$DOMAIN_ORIG"
printf '\n// drift\n' >>"$DOMAIN"
out=$(MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉞b")
expect 1 $? "㉞ 基线之后 path 依赖(agent24-domain)变了 → 同样拒绝"
expect yes "$([[ $out == *源码变了* ]] && echo yes || echo no)" "㉞ 且理由是「源码变了」"
cp "$DOMAIN_ORIG" "$DOMAIN"; rm -f "$DOMAIN_ORIG"
TOML=$CRATE_DIR/Cargo.toml; TOML_ORIG=$(mktemp); cp "$TOML" "$TOML_ORIG"
printf '\n# drift\n' >>"$TOML"
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉞c" >/dev/null
expect 1 $? "㉞ 基线之后 Cargo.toml 变了 → 拒绝"
cp "$TOML_ORIG" "$TOML"; rm -f "$TOML_ORIG"
# ㉞d 跑测试期间别的源码被改 → 作废
MUT_CARGO=$FAKE/cond MUT_PAUSE_AT=after-run python3 docs/agent/mutate.py mut "$F" "$BREAKER" '        if false {' x >/dev/null 2>&1 &
pypid=$!
wait_for "$GITDIR/mutate-paused-after-run"
printf '\n// drift during run\n' >>"$FRAME"
: >"$GITDIR/mutate-continue-after-run"
{ wait "$pypid"; } 2>/dev/null
expect 20 $? "㉞ 跑测试期间别的源码被改了 → 作废(20)"
cp "$FRAME_ORIG" "$FRAME"
# ㉞e 立基线期间源码被改 → 拒绝
MUT_CARGO=$FAKE/cond MUT_PAUSE_AT=baseline-after-run python3 docs/agent/mutate.py baseline agent24-os-proto >/dev/null 2>&1 &
pypid=$!
wait_for "$GITDIR/mutate-paused-baseline-after-run"
printf '\n// drift during baseline\n' >>"$FRAME"
: >"$GITDIR/mutate-continue-baseline-after-run"
{ wait "$pypid"; } 2>/dev/null
expect 1 $? "㉞ 立基线期间源码被改了 → 拒绝"
cp "$FRAME_ORIG" "$FRAME"
MUT_CARGO=$FAKE/cond mut_baseline agent24-os-proto >/dev/null || { echo "  假基线立不起来"; exit 1; }
mkdir -p "$GITDIR/mutate-inflight.tmp-stale" && echo junk >"$GITDIR/mutate-inflight.tmp-stale/bak"
MUT_CARGO=$FAKE/cond mut "$F" "$BREAKER" '        if false {' "㉟" >/dev/null
expect RED "$_MUT_LAST" "㉟ 写到一半的日志(临时目录)不挡路 —— 那时源码没被动过"
expect no "$([ -e "$GITDIR/mutate-inflight.tmp-stale" ] && echo yes || echo no)" "㉟ 且被清掉"

MUT_CARGO=$FAKE/failed0 mut "$F" "$BREAKER" '        if false {' "㉟b" >/dev/null
expect VOID "$_MUT_LAST" "㉟b FAILED 却 0 failed → 作废,不是红"
MUT_CARGO=$FAKE/tworun mut "$F" "$BREAKER" '        if false {' "㉟c" >/dev/null
expect VOID "$_MUT_LAST" "㉟c 两行 Running 一行崩溃 → 认不出是谁,作废"
echo 0 >"$FAKE/cnt"
MUT_CARGO=$FAKE/names mut "$F" "$BREAKER" '        if false {' "㉟d" >/dev/null
expect VOID "$_MUT_LAST" "㉟d 两次都红但失败的不是同一批测试 → 作废"

echo "── SIGKILL 之后的 recover ──"
kill9_mid_run() { # 跑一格,在测试进行中 SIGKILL 掉 python,留下日志
  : >"$FAKE/pids"; rm -f "$FAKE/running"
  run_bg "$FAKE/slow" ""
  wait_for "$FAKE/running"
  # 只杀这棵树的 mutate.py —— 全局的 `pkill -f "mutate.py mut"` 会连同一台机器上别的
  # worktree 里正在跑的变异一起杀掉(实际发生过:另一棵树的变异被打断在半路)。
  pkill -9 -f "$ROOT/docs/agent/mutate.py mut" ; { wait "$BG"; } 2>/dev/null
  xargs kill -9 <"$FAKE/pids" 2>/dev/null
}
kill9_mid_run
expect present "$(journal)" "㊱ SIGKILL 之后日志还在"
expect changed "$(same_as_orig)" "㊱ 且源码仍是变异状态(前提)"
mut_recover >/dev/null
expect 0 $? "㊱ 没人动过 → recover 成功"
expect same "$(same_as_orig)" "㊱ 且恢复成原文"
kill9_mid_run
printf '\n// USER WORK AFTER THE CRASH\n' >>"$F"
mut_recover >/dev/null
expect 2 $? "㊲ 崩溃后有人改过 → recover 拒绝(返回 2)"
expect yes "$(grep -q 'USER WORK AFTER THE CRASH' "$F" && echo yes || echo no)" "㊲ 且用户的修改还在"
expect present "$(journal)" "㊲ 且备份保留"
echo corrupted >"$GITDIR/mutate-inflight/bak"
mut_recover >/dev/null
expect 1 $? "㊳ 备份与记录的 sha 不符 → 不据它写"
expect yes "$(grep -q 'USER WORK AFTER THE CRASH' "$F" && echo yes || echo no)" "㊳ 源码没被动"
rm -rf "$GITDIR/mutate-inflight"; cp "$ORIG" "$F"
# ㊳a SIGKILL 落在我们写到一半时(日志里有 writing 标记),之后又有人改过这个文件:recover 写回
# 原文,但**先把此刻的内容存档**,并说出存在哪(复审 @ #172 F3)。
kill9_mid_run
: >"$GITDIR/mutate-inflight/writing"
printf '\n// USER EDIT AFTER A HALF WRITE\n' >>"$F"
rm -f "$GITDIR"/mutate-overwritten-*
out=$(mut_recover)
expect 0 $? "㊳a writing 标记在 → recover 写回原文"
expect same "$(same_as_orig)" "㊳a 源码是原文"
expect yes "$(grep -l 'USER EDIT AFTER A HALF WRITE' "$GITDIR"/mutate-overwritten-* >/dev/null 2>&1 && echo yes || echo no)" "㊳a 覆盖前的内容已存档"
expect yes "$([[ $out == *mutate-overwritten-* ]] && echo yes || echo no)" "㊳a 且提示里说出了存档位置"
rm -f "$GITDIR"/mutate-overwritten-*
kill9_mid_run
rm -f "$F"
out=$(mut_recover)
expect 2 $? "㊳b 源文件被删了 → recover 返回 2 并说明,不重建、不绕圈"
expect yes "$([[ $out == *不存在了* ]] && echo yes || echo no)" "㊳b 且说的是「不存在了」,不是「恢复失败,去 recover」"
cp "$ORIG" "$F"; rm -rf "$GITDIR/mutate-inflight"
# ㊳c 我们自己写到一半(RLIMIT_FSIZE):不是「有人改过」,当场就能收拾
BIG=$(python3 -c "print('        if false { /* ' + 'x' * 4000 + ' */')")
# bash 的 ulimit -f 以 1024 字节为单位(不是 POSIX 的 512)。限额 = 原文 + 不到 1 KiB:
# 备份(与原文等长)写得下,4 KiB 长的变异写不下。
LIMIT=$(( ($(wc -c <"$F") + 600) / 1024 + 1 ))
expect yes "$([ $((LIMIT * 1024)) -lt $(( $(wc -c <"$F") + ${#BIG} - ${#BREAKER} )) ] && echo yes || echo no)" "㊳c (前提)变异确实超出限额"
( ulimit -f "$LIMIT"; MUT_CARGO=$FAKE/cond python3 docs/agent/mutate.py mut "$F" "$BREAKER" "$BIG" x >/dev/null 2>&1 )
rc=$?
expect same "$(same_as_orig)" "㊳c 写变异写到一半失败 → 源码被恢复(那半截是我们写的)"
expect none "$(journal)" "㊳c 且日志已清"
expect 3 "$rc" "㊳c 退出码 3(内部错误),源码确已恢复 —— 3 的承诺这次是真的"
rm -rf "$GITDIR/mutate-inflight"; cp "$ORIG" "$F"
# ㊳d 内部错误 + 恢复失败同时发生:报 2(源码没恢复),不能报 3(3 的承诺是「源码已恢复」)
MUT_CARGO=/nonexistent/cargo MUT_PAUSE_AT=after-inject python3 docs/agent/mutate.py mut "$F" "$BREAKER" '        if false {' x >/dev/null 2>&1 &
pypid=$!
wait_for "$GITDIR/mutate-paused-after-inject"
printf '\n// EDIT WHILE INJECTED\n' >>"$F"
: >"$GITDIR/mutate-continue-after-inject"
{ wait "$pypid"; } 2>/dev/null
expect 2 $? "㊳d 抛了内部错误、而源码又恢复不了 → 2,不是 3"
rm -rf "$GITDIR/mutate-inflight"; cp "$ORIG" "$F"
run_bg "$FAKE/cond" after-run
wait_for "$GITDIR/mutate-paused-after-run"
printf '\n// EDITOR SAVE DURING THE RUN\n' >>"$F"
: >"$GITDIR/mutate-continue-after-run"
{ wait "$BG"; } 2>/dev/null
expect 2 $? "㊴ 跑测试期间有人改了源码 → 不覆盖,返回 2"
expect 1 "$(grep -c 'if false {' "$F")" "㊴ (前提)变异确实还在文件里 —— 所以提示必须说出来"
expect yes "$(grep -q 'EDITOR SAVE DURING THE RUN' "$F" && echo yes || echo no)" "㊴ 且那次修改还在"
rm -rf "$GITDIR/mutate-inflight"; cp "$ORIG" "$F"
# ㊴b 同样的冲突,再叠一个晚到的 TERM:退出码必须是 2(没恢复),不能是 143(「已恢复」)
MUT_CARGO=$FAKE/cond MUT_PAUSE_AT=after-run python3 docs/agent/mutate.py mut "$F" "$BREAKER" '        if false {' x >"$FAKE/conflict.out" 2>&1 &
pypid=$!
wait_for "$GITDIR/mutate-paused-after-run"
printf '\n// EDITOR SAVE\n' >>"$F"
kill -TERM "$pypid"; { wait "$pypid"; } 2>/dev/null
expect 2 $? "㊴b 冲突 + 晚到的信号 → 2,不是 143"
expect yes "$(grep -q '仍在文件里' "$FAKE/conflict.out" && echo yes || echo no)" "㊴b 冲突提示说明变异还在文件里"
rm -rf "$GITDIR/mutate-inflight"; cp "$ORIG" "$F"
# ㊸ 日志指向树外的文件:不写
mkdir -p "$GITDIR/mutate-inflight"
OUTSIDE=$(mktemp); echo outside >"$OUTSIDE"; cp "$OUTSIDE" "$GITDIR/mutate-inflight/bak"
python3 - "$GITDIR/mutate-inflight" "$OUTSIDE" <<'PY'
import hashlib, json, sys
d, path = sys.argv[1], sys.argv[2]
bak = open(d + "/bak", "rb").read()
json.dump({"path": path, "original_sha": hashlib.sha256(bak).hexdigest(), "mutated_sha": "x"}, open(d + "/meta.json", "w"))
PY
echo "changed-by-someone" >"$OUTSIDE"
mut_recover >/dev/null
expect 1 $? "㊸ 日志指向这棵树之外 → recover 拒绝"
expect changed-by-someone "$(cat "$OUTSIDE")" "㊸ 且那个文件没被写"
rm -rf "$GITDIR/mutate-inflight" "$OUTSIDE"
# ㊹ 脚手架内部错误:退出码 3,不和「拒绝开始」(1)混在一起;源码照样恢复
MUT_CARGO=/nonexistent/cargo mut "$F" "$BREAKER" '        if false {' "㊹" >/dev/null
expect 3 $? "㊹ 内部错误 → 3"
expect same "$(same_as_orig)" "㊹ 且源码已恢复"
expect none "$(journal)" "㊹ 且日志已清"

echo "── 调用方的 shell ──"
got=$(bash -c "set -e; cd $(printf %q "$ROOT") && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/ok") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' x >/dev/null; echo AFTER:\$_MUT_LAST")
expect "AFTER:ALIVE" "$got" "㊵ set -e 的调用方:一格读成 ALIVE 不会让脚本退出"
: >"$FAKE/pids"; rm -f "$FAKE/running"
bash -c "trap 'echo CALLER_TERM >$(printf %q "$FAKE/caller"); exit 7' TERM; cd $(printf %q "$ROOT") && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/slow") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' x" >/dev/null 2>&1 &
cpid=$!
wait_for "$FAKE/running"
kill -TERM "$cpid"; { wait "$cpid"; } 2>/dev/null
expect 7 $? "㊶ 调用方自己的 TERM trap 照常运行(它 exit 7)"
expect CALLER_TERM "$(cat "$FAKE/caller" 2>/dev/null)" "㊶ 且 trap 的内容执行了"
expect same "$(same_as_orig)" "㊶ 源码已恢复"
: >"$FAKE/pids"; rm -f "$FAKE/running"
bash -c "cd $(printf %q "$ROOT") && source docs/agent/mutate.sh && MUT_CARGO_META=$(printf %q "$FAKE/slowmeta") mut_baseline agent24-os-proto" >/dev/null 2>&1 &
cpid=$!
wait_for "$FAKE/running"
kill -TERM "$cpid"; { wait "$cpid"; } 2>/dev/null
expect 143 $? "㊷ 立基线时(查编译范围那一步)收到 TERM → 以 143 退出"
expect dead "$(all_dead)" "㊷ 且那一步起的进程已被杀"

echo
expect same "$(same_as_orig)" "收尾:被变异的文件与开始时逐字节相同"
expect none "$(journal)" "收尾:没有未收尾的日志"
if [ "$bad" -ne 0 ]; then
  echo "⛔ 自证失败 —— 脚手架本身坏了,这一轮所有变异读数作废(而不是去调这个自证)"
  exit 1
fi
echo "✓ 自证全部通过"
