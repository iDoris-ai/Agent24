#!/usr/bin/env bash
# ---- fail-closed（w38）：历史研究脚本，未通过安全验收（见 docs/agent/cloud-wave38）。----
# 已知缺陷：无校验 rm -rf / 复用已有目录 / 删除预存 stop / 跟随 OUT 符号链接覆写。
# 没有任何环境变量或参数可以解除此拒绝；只能作为阅读材料保留。
echo "refused: 历史脚本 $(basename "$0") 已停用（未安全验收），不执行任何操作" >&2
exit 3
# ---- 以下为历史内容，保持原样，永不执行 ----
# 运行 rustdoc E0463 区分性实验。所有 cargo 子进程走 env -i allowlist + 假 HOME/CARGO_HOME/XDG/TMP。
# 用法: run-experiments.sh <WORKDIR> [TOOLCHAIN]   例: run-experiments.sh /tmp/rdrepro 1.95.0
# 不读取用户 Cargo config/credentials；fixture 无外部依赖，全部 --offline。
set -uo pipefail
WORK=${1:?usage: run-experiments.sh <WORKDIR> [TOOLCHAIN]}
TC=${2:-1.95.0}
HERE=$(cd "$(dirname "$0")" && pwd)
rm -rf "$WORK"; mkdir -p "$WORK"; WORK=$(cd "$WORK" && pwd)
FAKE="$WORK/fakehome"; mkdir -p "$FAKE/home" "$FAKE/cargo" "$FAKE/xdg" "$FAKE/tmp" "$WORK/logs"
bash "$HERE/make-fixture.sh" "$WORK/fx" >/dev/null
T="$WORK/shared-target"
LOG="$WORK/logs"
RUSTUP_HOME_REAL=${RUSTUP_HOME:-$HOME/.rustup}
TOOLBIN=$(dirname "$(command -v rustup || command -v cargo)")

c() { # c <ws-dir> <target-dir> [extra env...] -- cargo args...
  local dir=$1 tgt=$2; shift 2; local envs=()
  while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
  (cd "$dir" && env -i PATH="$TOOLBIN:/usr/bin:/bin" HOME="$FAKE/home" CFFIXED_USER_HOME="$FAKE/home" \
     CARGO_HOME="$FAKE/cargo" RUSTUP_HOME="$RUSTUP_HOME_REAL" RUSTUP_TOOLCHAIN="$TC" \
     XDG_CONFIG_HOME="$FAKE/xdg" TMPDIR="$FAKE/tmp" CARGO_TARGET_DIR="$tgt" CARGO_TERM_COLOR=never \
     "${envs[@]}" cargo "$@")
}
summ() { grep -E '^test result|error\[E[0-9]+\]|error: extern location|Blocking waiting|found possibly newer|Running|Doc-tests' "$1" | sed 's/^/    /' | head -20; }
rec() { echo "## $1 exit=$2"; summ "$3"; }

echo "# toolchain: $(env -i PATH="$TOOLBIN:/usr/bin:/bin" RUSTUP_HOME="$RUSTUP_HOME_REAL" RUSTUP_TOOLCHAIN="$TC" HOME="$FAKE/home" rustdoc -V)"
echo "# uname: $(uname -sm)"

# E1 正控制：单 workspace，cargo test --doc
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline --doc -vv >"$LOG/e1.log" 2>&1; rec "E1 ws_a test --doc" $? "$LOG/e1.log"
# E2 正控制：单 workspace，完整 cargo test
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline >"$LOG/e2.log" 2>&1; rec "E2 ws_a test(full)" $? "$LOG/e2.log"
# E3 第二个 source 路径共享同一 target：哈希比较
c "$WORK/fx/ws_b" "$T" -- test --workspace --offline >"$LOG/e3.log" 2>&1; rec "E3 ws_b test(full) shared target" $? "$LOG/e3.log"
echo "## E3 deps 文件（同名=共享产物，可被另一 source 路径的 cargo 覆写）:"
ls "$T/debug/deps" | grep -E '^lib(fake_async_trait|fx_store|fx_agent)-' | sort | sed 's/^/    /'

# E4 锁行为：ws_a 完整 test 进入慢集成测试窗口后，ws_b 的 cargo 是否被 build-dir 锁阻塞
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=8 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e4a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
t0=$(date +%s.%N)
c "$WORK/fx/ws_b" "$T" -- build --workspace --offline >"$LOG/e4b.log" 2>&1; eb=$?
t1=$(date +%s.%N); wait $PA; ea=$?
echo "## E4 hook_seen=$([ -f "$WORK/hook" ] && echo yes || echo no) ws_b build exit=$eb took=$(echo "$t1 - $t0" | bc)s (若≈0s 且无 Blocking => 测试执行阶段未持有 build-dir 锁)"
summ "$LOG/e4b.log"; rec "E4 ws_a test(full,sleep8)" $ea "$LOG/e4a.log"

# E5 自然写者：测试窗口内，另一 source 路径用不同 RUSTFLAGS 重建共享 proc-macro（文件名是否不变、inode 是否变化）
pm_so() { ls "$T"/debug/deps/libfake_async_trait-*.so "$T"/debug/deps/libfake_async_trait-*.dylib 2>/dev/null; }
echo "## E5 before: $(for f in $(pm_so); do stat -c '%n ino=%i' "$f" 2>/dev/null || stat -f '%N ino=%i' "$f"; done)"
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=8 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e5a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
c "$WORK/fx/ws_b" "$T" RUSTFLAGS="--cfg fx_other" -- build --workspace --offline >"$LOG/e5b.log" 2>&1; eb=$?
echo "## E5 ws_b RUSTFLAGS=--cfg fx_other build exit=$eb during window"
echo "## E5 after-writer: $(for f in $(pm_so); do stat -c '%n ino=%i' "$f" 2>/dev/null || stat -f '%N ino=%i' "$f"; done)"
wait $PA; rec "E5 ws_a test(full) with concurrent RUSTFLAGS writer" $? "$LOG/e5a.log"

# E6 正控制（注入）：测试窗口内 *用不同源码* 重建共享 proc-macro 到同名文件（模拟"另一 checkout 不同版本/补丁的同 PackageId"）
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=8 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e6a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
sed -i.bak 's#// REBUILD_SALT#// REBUILD_SALT\npub(crate) fn _salt() {}#' "$WORK/fx/shared/fake_async_trait/src/lib.rs"
c "$WORK/fx/ws_b" "$T" -- build --workspace --offline >"$LOG/e6b.log" 2>&1; eb=$?
echo "## E6 ws_b rebuild with changed shared pm source exit=$eb; after: $(for f in $(pm_so); do stat -c '%n ino=%i' "$f" 2>/dev/null || stat -f '%N ino=%i' "$f"; done)"
wait $PA; rec "E6 ws_a test(full) with concurrent pm-source writer" $? "$LOG/e6a.log"
mv "$WORK/fx/shared/fake_async_trait/src/lib.rs.bak" "$WORK/fx/shared/fake_async_trait/src/lib.rs"

# E7 正控制（注入）：测试窗口内删除 fx_store rlib（模拟早轮 libagent24_store-*.rlib 不存在）
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=6 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e7a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
SR=$(grep -o "fx_store=[^ ']*\.rlib" "$LOG/e1.log" | head -1 | cut -d= -f2)
[ -n "$SR" ] && mv "$SR" "$SR.hidden"
wait $PA; rec "E7 ws_a test(full) with store rlib removed in window ($SR)" $? "$LOG/e7a.log"
[ -n "$SR" ] && mv "$SR.hidden" "$SR"

# E9 版本偏斜写者：另一 source 路径（ws_b，模拟另一个 worktree/另一 commit）只重建 fx_store 且其源码不同 -> 同名 rlib、不同 SVH
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=6 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e9a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
cp "$WORK/fx/ws_b/store/src/lib.rs" "$WORK/store.lib.rs.orig"; echo 'pub fn ws_b_only() {}' >> "$WORK/fx/ws_b/store/src/lib.rs"
c "$WORK/fx/ws_b" "$T" -- build -p fx_store --offline >"$LOG/e9b.log" 2>&1; eb=$?
echo "## E9 ws_b build -p fx_store (skewed source) exit=$eb: $(grep -c Compiling "$LOG/e9b.log") compiled"
wait $PA; rec "E9 ws_a test(full) with skewed same-name store rlib" $? "$LOG/e9a.log"
cp "$WORK/store.lib.rs.orig" "$WORK/fx/ws_b/store/src/lib.rs"
# 恢复一致状态
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline --no-run >/dev/null 2>&1

# E10 proc-macro 偏斜写者：只重建共享 proc-macro（新 inode、同名、不同 SVH），不重建下游 rlib —— 对应"仅观察到 async_trait dylib 换 inode/mtime"
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=6 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e10a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
cp "$WORK/fx/shared/fake_async_trait/src/lib.rs" "$WORK/pm.lib.rs.orig"
echo 'fn _skew() {}' >> "$WORK/fx/shared/fake_async_trait/src/lib.rs"
c "$WORK/fx/ws_b" "$T" -- build -p fake_async_trait --offline >"$LOG/e10b.log" 2>&1; eb=$?
echo "## E10 ws_b build -p fake_async_trait exit=$eb; after: $(for f in $(pm_so); do stat -c '%n ino=%i' "$f" 2>/dev/null || stat -f '%N ino=%i' "$f"; done)"
wait $PA; rec "E10 ws_a test(full) with skewed same-name proc-macro dylib" $? "$LOG/e10a.log"
cp "$WORK/pm.lib.rs.orig" "$WORK/fx/shared/fake_async_trait/src/lib.rs"
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline --no-run >/dev/null 2>&1

# E11 同 E10 写者，但读者是 cargo test --doc（无慢测试窗口）：写者在读者之前完成 -> cargo 重新指纹 -> 预期通过（解释为何 --doc 单跑为绿）
cp "$WORK/fx/shared/fake_async_trait/src/lib.rs" "$WORK/pm.lib.rs.orig"; echo 'fn _skew2() {}' >> "$WORK/fx/shared/fake_async_trait/src/lib.rs"
c "$WORK/fx/ws_b" "$T" -- build -p fake_async_trait --offline >/dev/null 2>&1
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline --doc >"$LOG/e11.log" 2>&1; rec "E11 ws_a test --doc after same skew (writer finished first)" $? "$LOG/e11.log"
cp "$WORK/pm.lib.rs.orig" "$WORK/fx/shared/fake_async_trait/src/lib.rs"
c "$WORK/fx/ws_a" "$T" -- test --workspace --offline --no-run >/dev/null 2>&1

# E12 传递依赖缺席：窗口内让 fx_core rlib 暂时不存在（模拟写者重建传递依赖时的缺席瞬间）-> 预期 E0463 而非 "extern location"
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=6 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e12a.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
for CR in "$T"/debug/deps/libfx_core-*.rlib; do mv "$CR" "$CR.hidden"; done
wait $PA; rec "E12 ws_a test(full) with transitive fx_core rlib absent in window" $? "$LOG/e12a.log"
for CR in "$T"/debug/deps/libfx_core-*.rlib.hidden; do mv "$CR" "${CR%.hidden}"; done

# E12b 同 E12 但 rlib+rmeta 都缺席（写者重建期间 -L 目录内找不到该名字的任何候选） 暂时不存在（模拟写者重建传递依赖时的缺席瞬间）-> 预期 E0463 而非 "extern location"
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=6 FX_HOOK="$WORK/hook" -- test --workspace --offline >"$LOG/e12b.log" 2>&1 & PA=$!
for _ in $(seq 1 200); do [ -f "$WORK/hook" ] && break; sleep 0.1; done
for CR in "$T"/debug/deps/libfx_core-*.r*; do mv "$CR" "$CR.hidden"; done
wait $PA; rec "E12b ws_a test(full) with transitive fx_core rlib absent in window" $? "$LOG/e12b.log"
for CR in "$T"/debug/deps/libfx_core-*.r*.hidden; do mv "$CR" "${CR%.hidden}"; done

# E13 缺席窗口探针：在共享 target 上重建 proc-macro / 传递 rlib 时，以 ~1ms 间隔采样"文件是否存在"和 inode
probe() { # probe <glob> <out>
  local miss=0 n=0 inos=""
  while [ ! -f "$WORK/probe.stop" ]; do n=$((n+1)); f=$(ls $1 2>/dev/null | head -1)
    if [ -z "$f" ]; then miss=$((miss+1)); else i=$(stat -c %i "$f" 2>/dev/null || stat -f %i "$f"); case " $inos " in *" $i "*) ;; *) inos="$inos $i";; esac; fi
  done; echo "samples=$n missing=$miss inodes=[$inos ]" > "$2"; }
for tgtglob in "libfake_async_trait-*.so" "libfx_core-*.rlib"; do
  rm -f "$WORK/probe.stop"; probe "$T/debug/deps/$tgtglob" "$LOG/probe.out" & PP=$!
  echo "// salt $RANDOM" >> "$WORK/fx/shared/fake_async_trait/src/lib.rs"; echo "// salt $RANDOM" >> "$WORK/fx/ws_b/core/src/lib.rs"
  c "$WORK/fx/ws_b" "$T" -- build --workspace --offline >/dev/null 2>&1
  touch "$WORK/probe.stop"; wait $PP; echo "## E13 rebuild probe $tgtglob: $(cat "$LOG/probe.out")"
done

# E8 反控制：同样的窗口但不做任何写入
rm -f "$WORK/hook"
c "$WORK/fx/ws_a" "$T" FX_SLEEP=3 -- test --workspace --offline >"$LOG/e8.log" 2>&1; rec "E8 ws_a test(full,sleep3) no writer" $? "$LOG/e8.log"
echo "# logs: $LOG"
