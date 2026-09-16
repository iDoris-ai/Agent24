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
#
# 行首锚定认的是「物理行」，不是 Rust 定义：注释里、字符串里的一行
# `pub fn supervise() {}` 同样在行首，会报「已交付」—— 更糟的那个方向（复审
# PROBE 第 1 轮）。所以先把注释与字面量去掉再 grep（`code_only`）。
#
# 它必须是**一遍、从左到右**的词法扫描：分两遍（先 raw string 后块注释，或反
# 过来）时，一遍会吞掉另一遍的边界 —— 块注释里的 `r#"` 吃掉注释的 `*/`，字符串
# 里的 `/*` 吃掉后面的代码（复审 PROBE 第 2、3 轮各找到一种）。一个交替式、按
# 最左匹配扫下去，谁先开始谁整个被消费，这就是词法器的语义：行注释、嵌套块注释
# （递归 `(?&cmt)`）、raw string（r / br / cr，任意个 #）、普通 / 字节 / C 字符
# 串、字符字面量（先认它，免得 '"' 开出一个字符串；生命周期 'a 不匹配）。去掉
# 时只留下其中的换行，行结构不变。
#
# 前提与余下的：假定文件是能编译的 Rust（读的是 main）。一个没闭合的块注释不
# 合法，从它起到文件尾都当注释（便宜的方向）；未闭合的 `/*` 很多时扫描是平方
# 级的，只在不合法的输入上。仍会被认成交付的（更糟的方向，接受）：被
# `#[cfg(...)]` 关掉的定义、写在 `macro_rules!` 体里或任意宏调用体里的定义
# （宏展开不看，只看词法上写了什么）。要更准就得真的解析 Rust，这个脚本不做。
#
# 替换时垫一个空格在两端（复审 PROBE 第 4 轮）：去掉的内容如果紧挨着它前后的
# 字符，两边拼起来可能凭空造出一个 `/*`——例如合法代码 `X/""*X`（`X` 重载了
# `Div<&str>`）里空字符串被删空后，`/` 和 `*` 直接贴上，被下面「未闭合块注释
# 到文件尾」那条规则整段吃掉，把后面几百行真代码一起删掉。垫的空格只在匹配的
# 两端各一个（不是每行），断开这种拼接；真正**原文里**就连着的 `/*` `*/`
# `//`——不是替换拼出来的——本来就会被词法当成注释/字符串边界、被上面那条
# 交替式自己的分支吃掉，不会走到这里，所以两端各垫一个空格不会漏放过任何真正
# 未闭合的块注释。
code_only() { # stdin → stdout：去掉注释与字面量，只留其中的换行
  perl -0777 -pe '
    s{
        //[^\n]*
      | (?<cmt> /\* (?: [^/*]++ | /(?!\*) | \*(?!/) | (?&cmt) )* \*/ )
      | (?<![A-Za-z0-9_]) [bc]?r (?<h>\#*) " .*? " \k<h>
      | [bc]? " (?: \\. | [^"\\] )*+ "
      | \x27 (?: \\. | [^\x27\\\n] ) \x27
    }{ (my $t = $&) =~ tr/\n//cd; " " . $t . " " }gsxe;
    s{/\*.*\z}{}s;
  '
}
has_symbol() { # has_symbol <符号>  —— 从 stdin 读内容
  code_only | grep -qE "^[[:space:]]*$1\b"
}
command -v perl >/dev/null 2>&1 || {
  echo "⛔ 需要 perl（去掉注释与字面量再认符号）。**不把「认不了」当成「没有」**。"
  exit 2
}

REF=${ME3_REF:-origin/main}

probe() { # probe <描述> <文件> <符号>
  local desc=$1 file=$2 sym=$3 on_ref=1 on_tree=1
  git show "$REF:$file" 2>/dev/null | has_symbol "$sym" && on_ref=0
  [ -f "$file" ] && has_symbol "$sym" < "$file" && on_tree=0

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
  # 文件已在 REF、符号却不在：要么还没写（刚开工的空壳，或者这一刀落在一个早就
  # 存在的文件里，如 3g 的 domain.rs），要么是这一行的符号预言过期了
  # （交付用了别的名字）。两者探针分不清，所以不下结论，只点名要人核对 ——
  # 「预言过期」已经发生过两次，每次都是报「未开工」报了好几个 PR 才被发现。
  if git cat-file -e "$REF:$file" 2>/dev/null; then
    echo "  ◌ 未开工?    $desc   (文件已在 $REF,但找不到 \`$sym\` —— 还没写,还是这一行的符号过期了?)"
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
# 会永远停在 ○ —— 那已经发生过两次:`3b-3 进程监督` 原本预言 `pub struct
# Supervisor`,而 #171 交付的是 `RestartPolicy` + `terminate_group`,于是它在
# supervise.rs 已经合进 main 之后仍报「未开工」;改成 `terminate_group` 之后,
# SUP-1…5 又把它和 `pub fn spawn` 一起改掉了(`spawn` 变成 `async`,组终止收进
# `ModuleProcess::stop`),两格又报了五个 PR 的「未开工」。
#
# **所以交付一刀时,改这一行是交付的一部分**,和写测试一样 —— 不是事后整理。
# 并且:符号写成对无关修饰宽容的形状(`pub (async )?fn`),文件已在 main 而符号
# 不在时探针报 ◌ 而不是 ○(见 probe),下面的自证在 REF 上探一遍这两个交付过的
# 符号 —— 这三件都是为了让「预言过期」不再安静。
probe "3a   发现与安装"          rust/crates/agent24-os-packages/src/install.rs "pub fn install"
probe "3b-1 framing"             rust/crates/agent24-os-proto/src/frame.rs       "pub fn read_frame"
probe "3b-2a 版本协商"           rust/crates/agent24-os-proto/src/version.rs     "pub fn negotiate"
probe "3b-2b initialize 线格式"  rust/crates/agent24-os-proto/src/initialize.rs  "pub fn accept"
probe "3b-3 manifest spawn 字段" rust/crates/agent24-domain/src/lib.rs           "pub struct SpawnCommand"
probe "3b-3 解析+起进程"         rust/crates/agent24-os-proto/src/launch.rs      "pub (async )?fn spawn"
probe "3b-3 进程监督"            rust/crates/agent24-os-proto/src/supervisor.rs  "pub fn supervise"
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
# 自证坏了要让退出码也坏：只打印 ✗ 而退出 0，调用它的脚本和 CI 看不见（复审
# PROBE 第 1 轮）。
SELFTEST_FAILED=0
say() {
  if [ "$1" = "$2" ]; then
    echo "  ✓ $3"
  else
    echo "  ✗ 探针坏了:$3(得到 $2,应为 $1)"
    SELFTEST_FAILED=1
  fi
}
# 自证跑在临时文件上,而临时文件不可能在 REF 里 —— 所以自证用的是同一条 grep
# 规则,不是同一个 probe。差别写出来:自证证的是**符号识别规则**,不是「读哪棵树」。
tprobe() { # tprobe <文件> <符号> —— 只测识别规则（与 probe 同一个 has_symbol）
  [ -f "$1" ] && has_symbol "$2" < "$1"
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

printf 'pub async fn spawn() {}\n' > "$tmp/async.rs"
tprobe "$tmp/async.rs" "pub (async )?fn spawn"
say 0 $? "async 定义被宽容形状探到(\`spawn\` 变成 async 曾让一格静默过期)"

# 上面几格只证识别规则,跑在临时文件上;这两格走真路径 —— 读 $REF —— 探两个
# 已知交付过的 SUP 符号。它们报 ✗ 说明「读 REF」这条路坏了,或者这两个符号又
# 被改名了 —— 两种都要有人来改上面的行,而不是让它们安静地报「未开工」。
#
# 只在默认的 origin/main 上跑：`ME3_REF` 指向 SUP 之前的某个提交时，这两个符号
# 本来就不在，报「探针坏了」是冤枉它（复审 PROBE 第 1 轮）。
if [ -z "${ME3_REF:-}" ]; then
  REF_SUP_OK=0
  for spec in "rust/crates/agent24-os-proto/src/launch.rs|pub (async )?fn spawn" \
              "rust/crates/agent24-os-proto/src/supervisor.rs|pub fn supervise"; do
    git show "$REF:${spec%%|*}" 2>/dev/null | has_symbol "${spec#*|}" || REF_SUP_OK=1
  done
  say 0 $REF_SUP_OK "已知交付的 SUP 符号在 $REF 上被探到(走真的读 REF 路径)"
else
  echo "  - 跳过「已知交付的 SUP 符号」:ME3_REF=$ME3_REF 是自定义坐标,那里未必已交付"
fi

printf '/*\npub fn commented() {}\n*/\n' > "$tmp/block.rs"
tprobe "$tmp/block.rs" "pub fn commented"
say 1 $? "符号只在块注释里 → 未开工(更糟的方向:不能报已交付)"

printf 'const S: &str = r#"\npub async fn spawn() {}\n"#;\n' > "$tmp/raw.rs"
tprobe "$tmp/raw.rs" "pub (async )?fn spawn"
say 1 $? "符号只在 raw string 里 → 未开工"

printf '/* 外\n/* 内 */\npub fn nested() {}\n*/\n' > "$tmp/nested.rs"
tprobe "$tmp/nested.rs" "pub fn nested"
say 1 $? "符号在嵌套块注释里 → 未开工(Rust 的块注释可以嵌套)"

printf 'const C: &CStr = cr#"\npub fn in_c_string() {}\n"#;\n' > "$tmp/craw.rs"
tprobe "$tmp/craw.rs" "pub fn in_c_string"
say 1 $? "符号只在 raw C 字符串(cr#\"…\"#)里 → 未开工"

# 复审第 3 轮的反例：注释里的 `r#"` 不能吃掉注释的边界。
printf '/*\npub fn leaked() {}\nr#" */\nconst S: &str = r#"ok"#;\n' > "$tmp/interleaved.rs"
tprobe "$tmp/interleaved.rs" "pub fn leaked"
say 1 $? "注释里写着 r#\" 也不会让注释漏出来 → 未开工"

printf 'const S: &str = "\npub fn in_string() {}\n";\n' > "$tmp/string.rs"
tprobe "$tmp/string.rs" "pub fn in_string"
say 1 $? "符号只在跨行的普通字符串里 → 未开工"

# 便宜方向的两格：字面量与行注释里的 `/*` 不许吞掉后面的真定义。
printf 'const G: &str = "src/*.rs";\nconst Q: char = \x27"\x27;\npub fn after_glob() {}\n' > "$tmp/glob.rs"
tprobe "$tmp/glob.rs" "pub fn after_glob"
say 0 $? "字符串里的 /* 与字符字面量 '\"' 之后的真定义仍被探到"

printf '// 见 /* 这里\npub fn after_line() {}\n' > "$tmp/line.rs"
tprobe "$tmp/line.rs" "pub fn after_line"
say 0 $? "行注释里的 /* 之后的真定义仍被探到"

# 复审第 4 轮的反例：删空的字面量不能把前后贴上拼出一个 /*，再被「未闭合
# 块注释到文件尾」吃掉后面的真代码。
printf 'fn f() { let _ = X/""*X; }\npub fn real() {}\n' > "$tmp/fused.rs"
tprobe "$tmp/fused.rs" "pub fn real"
say 0 $? "删空字面量拼出的 /* 不会吞掉后面的真定义(前后垫了空格断开)"

printf '/* 一段注释 */\npub fn after_comment() {}\n' > "$tmp/after.rs"
tprobe "$tmp/after.rs" "pub fn after_comment"
say 0 $? "块注释之后的真定义仍被探到(证明去注释没有吞掉后面的代码)"

exit $SELFTEST_FAILED
