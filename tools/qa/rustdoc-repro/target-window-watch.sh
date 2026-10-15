#!/usr/bin/env bash
# ---- fail-closed（w38）：历史研究脚本，未通过安全验收（见 docs/agent/cloud-wave38）。----
# 已知缺陷：无校验 rm -rf / 复用已有目录 / 删除预存 stop / 跟随 OUT 符号链接覆写。
# 没有任何环境变量或参数可以解除此拒绝；只能作为阅读材料保留。
echo "refused: 历史脚本 $(basename "$0") 已停用（未安全验收），不执行任何操作" >&2
exit 3
# ---- 以下为历史内容，保持原样，永不执行 ----
# 只读诊断：在原样运行 `cargo test --workspace --offline --locked -vv` 的同时，监视共享 target 中
# 指定 crate 的 rlib/rmeta/dylib/.d 与 .fingerprint 目录的 inode/mtime/size。runner 自己在编译结束后
# 不会再写这些文件；测试执行窗口内出现的任何变化 = 外部写者重建了“同名单元”。
# 不扫描进程/env，不修改 target。macOS(stat -f) 与 Linux(stat -c) 均可。
# 用法: target-window-watch.sh <TARGET_DIR> <OUT_LOG> [crate_prefix ...]
#   例: target-window-watch.sh rust/target /tmp/watch.log agent24_store agent24_agent async_trait agent24_core
#   结束: touch <OUT_LOG>.stop
set -u
T=${1:?target dir}; OUT=${2:?out log}; shift 2
PREFIXES=${*:-agent24_store agent24_agent async_trait}
if [ "$(uname -s)" = Darwin ]; then st() { stat -f '%i %m %z %N' "$@" 2>/dev/null; }
else st() { stat -c '%i %Y %s %n' "$@" 2>/dev/null; }; fi
snap() {
  for p in $PREFIXES; do
    dash=${p//_/-}
    st "$T"/debug/deps/lib"$p"-* "$T"/debug/deps/"$p"-*.d "$T"/debug/.fingerprint/"$dash"-*/* "$T"/debug/.fingerprint/"$p"-*/*
  done | sort -k4
}
rm -f "$OUT.stop"; prev=$(snap); { echo "# $(date '+%F %T') start"; echo "$prev"; } > "$OUT"
while [ ! -f "$OUT.stop" ]; do
  sleep 0.5; cur=$(snap)
  if [ "$cur" != "$prev" ]; then
    echo "# $(date '+%F %T') CHANGE" >> "$OUT"
    diff <(echo "$prev") <(echo "$cur") >> "$OUT"
    prev=$cur
  fi
done
echo "# $(date '+%F %T') stop" >> "$OUT"
