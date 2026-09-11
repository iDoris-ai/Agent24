#!/usr/bin/env bash
# ME-3 每一刀的状态 —— 由探针决定，不由人手写。
#
# 存在的理由：这份状态表上一版是手写的，在合并那一刻已经有三行是错的
# （PR 已合但表里写「复审中」、已交付的刀写「未开工」）。写的时候三行都对；
# 问题是没有任何东西让它们会过期。
#
# 判据（复审 @ #165 定的）：「这一行还准不准」必须是一条别人能跑的命令，
# 而不是一次阅读。
#
#   用法：bash docs/agent/me3-status.sh
#
# ── 探针的失效方向，以及为什么它比那张表更值得小心 ────────────────────
#
# 第一版探针有 7 行只查「文件在不在」、6 行用纯子串 grep。复审量出三格：
# 一个只有一行注释的 `supervise.rs` 报「已在 main」；把真的 `pub fn accept`
# 改名、文件头留一句 `// TODO: 这里以后会有 pub fn accept`，同样报「已在 main」。
#
# **而「建文件、写一句 TODO 点名将来那个函数」恰好是开工最自然的第一步。**
#
# 这个失效方向比旧表更糟：旧表错在「已交付却写未开工」→ 有人重做一件已完成的
# 事，浪费，但**去写的时候就会发现代码已经在那儿**；探针错在「未开工却报已
# 交付」→ 有人**跳过**这一刀，一直到 3f 的黑盒验收才暴露。
#
# 所以：**每一刀都必须有符号（没有只查文件在不在的行），符号必须行首锚定
# （注释里提到不算），并且下面的自证要覆盖这两种形态本身。**
set -u
cd "$(dirname "$0")/../.." || exit 1

# ── 它读的是 main，因为读者问的是关于 main 的问题 ──────────────────
#
# 第三处「它报的坐标不是它量的坐标」，而且这一处不是构造出来的：上一版行标签写
# `● 已在 main`，而 grep 读的是**脚本所在的那棵树**。在任何一棵落后于 main 的树
# 上跑它，答案都是错的，**而且是以「关于 main」的口吻说出来的** —— 复审在
# #165 自己的分支上跑，两刀已交付的工作被报成「未开工」。
#
# 失效方向和最初那张手写表一模一样：说「未开工」而其实已交付 → 有人去重做一件
# 已完成的事。**那句话就写在这个脚本的注释里。**
#
# 两条路：把标签改成「这棵树上有/没有」（最省），或者真去读 main。取后者，因为
# 读者要回答的是「我该不该写这一刀」—— 那是关于 main 的问题，改标签只是把问题
# 让给读者。
#
# 代价如实写：读的是 `origin/main`，也就是**最后一次 fetch 的状态**，不是此刻的
# 远端。所以表头报它的 sha 和它有多旧；取不到这个 ref 时**明确报错，不当作
# 「没有」** —— 那正是这一整族缺陷的形状。
#
# 行首锚定：`^[[:space:]]*<符号>` —— 一个定义在行首（可缩进），一句
# `// TODO: 将来会有 pub fn accept` 不在行首。
has_symbol() { # has_symbol <内容来源命令...> <符号>  —— 从 stdin 读内容
  grep -qE "^[[:space:]]*$1\b"
}

REF=${ME3_REF:-origin/main}

probe() { # probe <描述> <文件> <符号>
  local desc=$1 file=$2 sym=$3 on_ref=1 on_tree=1
  git show "$REF:$file" 2>/dev/null | has_symbol "$sym" && on_ref=0
  [ -f "$file" ] && grep -qE "^[[:space:]]*${sym}\b" "$file" 2>/dev/null && on_tree=0

  if [ $on_ref -eq 0 ]; then
    # 在 main 上有，但本地这棵树没有 —— 说出来，因为读者多半正准备去写它。
    if [ $on_tree -ne 0 ]; then
      echo "  ● 已在 $REF  $desc   ⚠️ 你这棵树上没有(落后于 $REF,先 rebase)"
    else
      echo "  ● 已在 $REF  $desc"
    fi
    return 0
  fi
  if [ $on_tree -eq 0 ]; then
    echo "  ◐ 只在本地   $desc   (你这棵树上有,$REF 上还没有 —— 未合并)"
    return 1
  fi
  echo "  ○ 未开工     $desc"
  return 1
}

# 坐标。三处「它报的坐标不是它量的坐标」都在这里被回答：
#   ① 手写表会静默过期            → 换成探针
#   ② 打印 HEAD 却读工作树        → 脏树标记（下面 tree_coord）
#   ③ 标签声称 main 却读当前树    → 探针改成真读 REF（见 probe）
if ! git rev-parse --verify --quiet "$REF" >/dev/null; then
  echo "⛔ 取不到 $REF —— 无法回答「这一刀在不在 main 上」。"
  echo "   先 git fetch。**不把「取不到」当成「没有」**：那正是这个脚本"
  echo "   存在所要防的那一类错误。"
  exit 2
fi
ref_sha=$(git rev-parse --short "$REF")
ref_age=$(git log -1 --format=%cr "$REF" 2>/dev/null || echo "?")
tree_coord=$(git rev-parse --short HEAD 2>/dev/null || echo "?")
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  tree_coord="${tree_coord}+未提交改动"
fi
echo "ME-3 状态"
echo "  交付判据读的是 ${REF} = ${ref_sha}（最后一次 fetch：${ref_age}，不是此刻的远端）"
echo "  你这棵树是 ${tree_coord}"
echo

# ⚠️ 未开工那几刀的符号是**预言**,不是读数。交付时若 API 用了别的名字,这一行
# 会永远停在 ○ —— 那已经发生过一次:`3b-3 进程监督` 原本预言 `pub struct
# Supervisor`,而 #171 交付的是 `RestartPolicy` + `terminate_group`,于是它在
# supervise.rs 已经合进 main 之后仍报「未开工」。
#
# **所以交付一刀时,改这一行是交付的一部分**,和写测试一样 —— 不是事后整理。
probe "3a   发现与安装"          rust/crates/agent24-os-packages/src/install.rs "pub fn install"
probe "3b-1 framing"             rust/crates/agent24-os-proto/src/frame.rs       "pub fn read_frame"
probe "3b-2a 版本协商"           rust/crates/agent24-os-proto/src/version.rs     "pub fn negotiate"
probe "3b-2b initialize 线格式"  rust/crates/agent24-os-proto/src/initialize.rs  "pub fn accept"
probe "3b-3 manifest spawn 字段" rust/crates/agent24-domain/src/lib.rs           "pub struct SpawnCommand"
probe "3b-3 解析+起进程"         rust/crates/agent24-os-proto/src/launch.rs      "pub fn spawn"
probe "3b-3 进程监督"            rust/crates/agent24-os-proto/src/supervise.rs   "pub fn terminate_group"
probe "3b-4 受约束代理"          rust/crates/agent24-os-proto/src/proxy.rs       "pub fn proxy_router"
probe "3b-5 两阶段热 disable"    rust/crates/agent24-os-proto/src/drain.rs       "pub enum DrainState"
probe "3c   回调通道其余部分"    rust/crates/agent24-os-proto/src/rpc.rs         "pub fn dispatch"
probe "3d   记忆回调"            rust/crates/agent24-os-proto/src/memory.rs      "pub fn handle_memory"
probe "3e   事件 + 审批"         rust/crates/agent24-os-proto/src/events.rs      "pub fn handle_event"
probe "3f   仓外包端到端"        rust/apps/agent24d/tests/me3f_blackbox.rs       "fn a_package_from_outside_the_repo"
probe "3g   启用路径准入"        rust/apps/agent24d/src/domain.rs                "fn admit_on_enable"
echo
echo "验收(3f)未通过之前,「支持第三方 OS」只是一句声称。"
echo "探针只回答「代码在不在」,回答不了「有没有生产调用方」—— 🟢 与 ✅ 的区别仍要人判断。"
echo
echo "--- 探针自证 ---"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
say() { [ "$1" = "$2" ] && echo "  ✓ $3" || echo "  ✗ 探针坏了:$3(得到 $2,应为 $1)"; }
# 自证跑在临时文件上,而临时文件不可能在 REF 里 —— 所以自证用的是同一条 grep
# 规则,不是同一个 probe。差别写出来:自证证的是**符号识别规则**,不是「读哪棵树」。
tprobe() { # tprobe <文件> <符号> —— 只测识别规则
  grep -qE "^[[:space:]]*$2\b" "$1" 2>/dev/null
}

tprobe rust/crates/agent24-domain/src/lib.rs "pub struct DomainOsManifest"
say 0 $? "已知存在的符号被探到"

tprobe rust/crates/agent24-domain/src/lib.rs "pub struct NoSuchSymbolEverXYZ"
say 1 $? "存在的文件里、不存在的符号 → 未开工"

# 复审量出的两种形态,各一格。没有这两格,上面那两格挡不住它们:
# 正对照查的是真符号,负对照查的是不存在的符号 —— 都没覆盖「文件在但是空壳」
# 和「符号只在注释里」。
printf '// TODO\n' > "$tmp/shell.rs"
tprobe "$tmp/shell.rs" "pub fn something"
say 1 $? "空壳文件(只有一行注释) → 未开工"

printf '// TODO: 这里以后会有 pub fn something\n' > "$tmp/comment.rs"
tprobe "$tmp/comment.rs" "pub fn something"
say 1 $? "符号只出现在注释里 → 未开工"

printf 'pub fn something() {}\n' > "$tmp/real.rs"
tprobe "$tmp/real.rs" "pub fn something"
say 0 $? "同名符号真的定义了 → 已交付(证明上面两格不是靠「拒绝一切」通过的)"

printf '    pub fn indented() {}\n' > "$tmp/indent.rs"
tprobe "$tmp/indent.rs" "pub fn indented"
say 0 $? "缩进的定义仍被探到(行首锚定不等于必须顶格)"
