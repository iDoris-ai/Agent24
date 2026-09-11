#!/usr/bin/env bash
# `mutate.sh` 自己的自证 —— 一个拦不住假结论的脚手架，会安静地把假结论当成读数。
#
#   bash docs/agent/mutate-selftest.sh        # 全部通过 exit 0,任何一格不符 exit 1
#
# 每一格都**断言**判定,而不是打印一段「读法」让人去对。**对照是必要的**:⑤⑦ 必须是 🔴,
# 否则一个「拒绝一切」的脚手架也能通过其余每一格。
#
# 真 cargo 的格子验「对真实输出判得对不对」;假 cargo(`MUT_CARGO`)的格子验那些用真 cargo
# 造不出来、或要等很久的形态:红基线、不认识的输出、挂住且带孙进程、跑到一半被 SIGTERM。
set -u
cd "$(dirname "$0")/../.." || exit 1
ROOT=$(pwd)
# shellcheck source=docs/agent/mutate.sh
source docs/agent/mutate.sh

F=rust/crates/agent24-os-proto/src/supervise.rs
BREAKER='        if self.consecutive >= BREAKER_THRESHOLD {'
ORIG=$(mktemp); cp "$F" "$ORIG"
FAKE=$(mktemp -d)
trap 'cp "$ORIG" "$F"; rm -rf "$ORIG" "$FAKE"' EXIT
bad=0
# 进程标记每次运行都不同:一次失败的运行留下的进程,不能让下一次的「有没有残留」判错。
M=$((40000 + RANDOM % 20000))
expect() { # expect <期望> <实际> <格名>
  if [ "$1" = "$2" ]; then printf "    ✓ %s\n" "$3"; else printf "    ✗ %s:期望 %s,实际 %s\n" "$3" "$1" "${2:-<空>}"; bad=1; fi
}
fake() { # fake <名字> <脚本体> → 路径
  printf '#!/usr/bin/env bash\n%s\n' "$2" >"$FAKE/$1"; chmod +x "$FAKE/$1"; echo "$FAKE/$1"
}

echo "── 真 cargo ──"
mut_baseline agent24-os-proto supervise || { echo "  基线立不起来,自证无法进行"; exit 1; }

mut "$F" 'NO-SUCH-ANCHOR' 'x' "① 锚点不存在";                       expect VOID "$_MUT_LAST" "① → 作废"
mut "$F" '    }' '    };' "② 锚点不唯一";                            expect VOID "$_MUT_LAST" "② → 作废"
mut "$F" 'pub const REAP_TIMEOUT' 'const UNUSED_XYZ: u32 = 7;
pub const REAP_TIMEOUT' "③ 纯插入(空操作)";                           expect VOID "$_MUT_LAST" "③ → 作废"
mut "$F" "$BREAKER" '        if self.consecutive >= BREAKER_THRESHOLD (' "④ 编译失败"
                                                                        expect VOID "$_MUT_LAST" "④ → 作废"
mut "$F" "$BREAKER" '        if false {' "⑤ 对照:真变异";                expect RED "$_MUT_LAST" "⑤ → 必须红"
mut "$F" '#[test]
    fn only_a_long_enough_run_counts_as_healthy' '#[allow(dead_code)]
    fn only_a_long_enough_run_counts_as_healthy' "⑥ 藏掉一条测试(B2)"
                                                                        expect VOID "$_MUT_LAST" "⑥ 测试数少了一条 → 作废,不是 🟢"
mut "$F" "$BREAKER" '        if { std::process::abort() } {' "⑦ 测试进程崩溃"
                                                                        expect RED "$_MUT_LAST" "⑦ 崩溃 → 红,不是「编译失败」"

mut rust/crates/agent24-os-proto/src/supervize_TYPO.rs 'a' 'b' "⑧ 路径打错" >/dev/null
expect 1 $? "⑧ 路径打错 → 拒绝"
expect no "$([ -e rust/crates/agent24-os-proto/src/supervize_TYPO.rs ] && echo yes || echo no)" "⑧ 且没有凭空建出文件"
mut rust/crates/agent24-domain/src/lib.rs 'a' 'b' "⑨ 不在基线 crate 里" >/dev/null
expect 1 $? "⑨ 文件不在基线 crate → 拒绝"

saved=$_MUT_BASE
_MUT_BASE="FAIL test result: FAILED. 37 passed; 4 failed"            # 红基线会留下的那个值
mut "$F" "$BREAKER" '        if false {' "⑩ 红基线(B1)" >/dev/null
expect 1 $? "⑩ 红基线上不许开始"
_MUT_BASE=$saved

mut_baseline agent24-os-proto nosuch_filter_typo_xyz >/dev/null
expect 1 $? "⑪ 0 个测试的基线 → 拒绝(B2)"
mut "$F" "$BREAKER" '        if false {' "⑪ 之后" >/dev/null
expect 1 $? "⑪ 且之前那条好基线不再生效"

echo "── 假 cargo ──"
OK8='echo "test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured"'
MUT_CARGO=$(fake red 'echo "test result: FAILED. 7 passed; 1 failed; 0 ignored"; exit 101') \
  mut_baseline agent24-os-proto >/dev/null
expect 1 $? "⑫ 真红的基线 → 拒绝(B1)"
expect "" "$_MUT_BASE" "⑫ 且 _MUT_BASE 为空"

MUT_CARGO=$(fake ok "$OK8") mut_baseline agent24-os-proto >/dev/null || { echo "  假基线立不起来"; exit 1; }
MUT_CARGO=$(fake weird 'echo "something unexpected"; exit 1') \
  mut "$F" "$BREAKER" '        if false {' "⑬ 不认识的输出"
expect VOID "$_MUT_LAST" "⑬ 不认识的输出 → 作废,不是 🔴"

MUT_TIMEOUT=2 MUT_CARGO=$(fake hang "sleep $M & sleep $((M+1)); wait") \
  mut "$F" "$BREAKER" '        if false {' "⑭ 挂住(带孙进程)"
expect TIMEOUT "$_MUT_LAST" "⑭ → 挂住"
sleep 1
expect "" "$(pgrep -f "sleep ($M|$((M+1)))\$")" "⑭ 且整组都被杀掉,没有留下孙进程"

# ⑮ B3:跑到一半被 SIGTERM。在一个独立的 bash 里跑,从外面杀它。
SLOW=$(fake slow "sleep $((M+2))")
bash -c "cd $(printf %q "$ROOT") && source docs/agent/mutate.sh \
  && MUT_CARGO=$(printf %q "$FAKE/ok") mut_baseline agent24-os-proto >/dev/null \
  && MUT_CARGO=$(printf %q "$SLOW") mut $(printf %q "$F") $(printf %q "$BREAKER") '        if false {' 'x'" \
  >/dev/null 2>&1 &
child=$!
for _ in $(seq 1 50); do pgrep -f "sleep $((M+2))\$" >/dev/null && break; sleep 0.2; done
expect changed "$(cmp -s "$F" "$ORIG" && echo same || echo changed)" "⑮ 前提:SIGTERM 之前源码确实处于被变异状态"
kill -TERM "$child"; { wait "$child"; } 2>/dev/null; rc=$?
expect 143 "$rc" "⑮ SIGTERM → 按原信号退出"
expect same "$(cmp -s "$F" "$ORIG" && echo same || echo changed)" "⑮ 源码已恢复(B3)"
sleep 1
expect "" "$(pgrep -f "sleep $((M+2))\$")" "⑮ 且测试进程组已被杀掉"

echo
expect same "$(cmp -s "$F" "$ORIG" && echo same || echo changed)" "收尾:被变异的文件与开始时逐字节相同"
if [ "$bad" -ne 0 ]; then
  echo "⛔ 自证失败 —— 脚手架本身坏了,这一轮所有变异读数作废(而不是去调这个自证)"; exit 1
fi
echo "✓ 自证全部通过"
