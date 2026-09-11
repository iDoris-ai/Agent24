#!/usr/bin/env python3
"""变异测试的执行核心 —— `mutate.sh` 是它的薄包装，平时用那个。

    python3 docs/agent/mutate.py baseline <crate> [测试过滤…]
    python3 docs/agent/mutate.py mut <文件> <锚点> <替换> <标签>
    python3 docs/agent/mutate.py recover        # 上次被 SIGKILL 之类打断时,恢复被变异的文件

退出码(`mutate.sh` 据此设 `$_MUT_LAST`):
    0 RED   变异被抓到          10 ALIVE  存活(判据不承重)
    20 VOID 读数作废            30 TIMEOUT 挂住
    1  拒绝开始(前置条件不满足)  2  源码恢复失败 —— 备份保留在 git-dir 里,见提示
    3  内部错误(脚手架自己出错;源码照样先恢复)
    128+n  被信号 n 打断(源码已恢复)

── 为什么从 bash 改成这个 ────────────────────────────────────────────────

第一版是 bash。两轮对抗复审(#172)找出的问题几乎全落在同一个根上:**用 shell 手搓一个
进程监督器**。trap 装得晚(注入之后)、撤得早(恢复之前),中间的窗口里一个信号就留下被
变异的源码;`$!` 拿到时子进程还没 setpgrp,超时去杀一个还不存在的进程组;超时靠复用
退出码 124 表达,一个自然退出 124 的命令被读成挂住;交互式 bash 里「清 trap 再 kill $$」
把整个 shell 杀掉(bash 3.2 实测)。这些不是再补几行能收住的 —— 每补一处就多一个窗口。

这里的做法:
  - **信号处理器只记一个标志,从不抛异常**。代码在明确的检查点看标志,所以没有「信号落在
    写到一半的地方」这种窗口;恢复这一步即使连按 Ctrl-C 也会做完。
  - 子进程 `start_new_session=True`:setsid 发生在 exec **之前**、Popen 返回之前,进程组
    在我们拿到 pid 时已经存在。超时由我们自己的时钟判,不看退出码。
  - 源码的备份先落进 git-dir 下的**日志**(临时目录里写全 + sha256 + fsync,再原子 rename
    成正式名字,所以不存在「写到一半的日志」),然后才动源码。源码**原地**写(保留 inode、
    属主、xattr、硬链接)。恢复前先认当前文件:是原文 → 已恢复;是我们写的变异 → 写回原文并
    逐字节比对;**两者都不是 → 有人在这期间改过它,不覆盖**,保留日志报冲突。绝不删唯一的
    备份。被 SIGKILL 打断后日志还在,下一次 baseline/mut 会拒绝开始并指向 `recover`。
  - 一把 `flock` 锁:同一棵树上两个变异不能同时跑(第二个会把第一个的变异当成原文备份,
    最后写回去)。
  - **能改的文件只限于 `--lib` 测试真正编译进去的那些** —— 从 cargo 的 dep-info 读出来,
    按规范化路径比。`src/bin/`、`tests/`、`build.rs`、没有 `mod` 声明的文件、`../` 与
    软链,都不在这份清单里。(前缀比较能被 `crate/../别处` 绕过,复审实测过。)
  - 🔴 不是一次读数说了算,两道确认:**变异版再跑一次**,要红得一样(同一类、同一批失败的
    测试)—— 否则是偶发;**恢复后的原样再跑一次**,要是那个全绿基线 —— 否则是基线漂移。
  - 基线记下所有被编译源码(+ Cargo.toml / Cargo.lock)的 sha256;`mut` 前后各核一次,树
    变了就拒绝或作废 —— 否则一个恰好把某条红测试「修好」的变异会读成 🟢,一个删掉了 `mod`
    声明的文件会被改了却没人编译。

── 它拦的假结论(每一条都在实际复审里发生过) ─────────────────────────

  1. 锚点不存在 / 不唯一 / 替换里仍完整包含锚点(纯插入)/ 改动落在 `//` 注释里(含行尾
     注释)→ 作废。
     纯插入那条也会拒绝 `x → !(x)` 这种包裹式变异,方向是保守的;换一个锚点即可。
  2. 编译失败(含 build script 失败)→ 作废。编译错误不是测试结果。
  3. 测试数与基线不一致(过滤词打错 → 0 个测试;`#[test]` 被变异藏掉)→ 作废。
     基线必须 > 0 个测试且全绿。
  4. 输出里 result 行不是恰好一行、或与退出码矛盾 → 作废,不落到 🔴 或 🟢。
  5. 测试进程崩溃(abort/段错误)算 🔴,但只在输出能确定崩的就是那个 lib 测试二进制时;
     别的子进程(build script、包装器)失败 → 作废。

**仍然拦不住的**,如实写:唯一锚点落在 `#[cfg(...)]` 编译掉的代码、字符串字面量、或块注释
里 → 假 🟢;测试进程另起会话的后代,只有在它父进程还活着时才能按 ppid 找到并杀掉 —— 父进程
先退出、后代被 launchd 收养的那种,找不到。选锚点时自己确认它是被编译、被执行的代码。

── 一个试过并否定的做法 ────────────────────────────────────────────────

曾想用「编译产物哈希是否变化」识别空操作。**实测不成立**:加一个没人用的 `const` 同样改变
哈希;同一份源码两次构建的哈希也不同。哈希能证明「有变化」,证明不了「行为有变化」。
"""

import errno
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

RED, ALIVE, VOID, TIMEOUT = 0, 10, 20, 30
REFUSED, RESTORE_FAILED, INTERNAL = 1, 2, 3

SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGQUIT, signal.SIGTERM)
_pending = []


def _on_signal(signum, _frame):
    # 只记标志。抛异常会让信号可以落在任何一行 —— 包括恢复写到一半的地方。
    _pending.append(signum)


def interrupted():
    return _pending[0] if _pending else None


def say(msg):
    print(msg, flush=True)


def git(*args, cwd=None):
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


class Refused(Exception):
    """前置条件不满足:什么都没动。"""


class Paths:
    def __init__(self):
        try:
            self.root = git("rev-parse", "--show-toplevel")
            self.gitdir = git("rev-parse", "--absolute-git-dir")
        except subprocess.CalledProcessError:
            raise Refused("不在 git 仓库里")
        self.rust = os.path.join(self.root, "rust")
        self.baseline = os.path.join(self.gitdir, "mutate-baseline.json")
        self.journal = os.path.join(self.gitdir, "mutate-inflight")
        self.lock = os.path.join(self.gitdir, "mutate.lock")


def take_lock(paths):
    fd = os.open(paths.lock, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as e:
        os.close(fd)
        if e.errno in (errno.EWOULDBLOCK, errno.EAGAIN):
            raise Refused("同一棵树上另一个 baseline/mut 正在跑 —— 并发会把别人的变异当成原文备份,拒绝")
        raise
    return fd  # 进程退出即释放


def check_no_journal(paths):
    # 临时目录 = 写日志时被打断:那时源码还没被动过(源码只在日志 rename 成正式名字之后才改),
    # 所以它里面没有任何需要恢复的东西。持锁时清掉。
    for name in os.listdir(paths.gitdir):
        if name.startswith("mutate-inflight.tmp-"):
            shutil.rmtree(os.path.join(paths.gitdir, name), ignore_errors=True)
    if os.path.isdir(paths.journal):
        target = ""
        try:
            with open(os.path.join(paths.journal, "meta.json"), encoding="utf-8") as f:
                target = json.load(f).get("path", "")
        except (OSError, ValueError):
            pass
        raise Refused(
            f"上一次变异没有收尾(被 SIGKILL 或恢复失败):{target or '<未知文件>'} 可能仍处于被变异状态。"
            f"先运行 `python3 docs/agent/mutate.py recover`"
        )


# ── 日志、写源码与恢复 ─────────────────────────────────────────────────
#
# 日志 = git-dir 下的 mutate-inflight/{bak, meta.json}。它先在同级的临时目录里写全、fsync,
# 再**原子 rename** 成正式名字 —— 所以一个「写到一半」的日志永远不会以正式名字存在(被
# SIGKILL 在写日志时打断,留下的只是一个临时目录,而那时源码还没被动过)。meta 里记着原文与
# 变异后内容的 sha256:恢复前先看当前文件是哪一个 —— 是原文就当已恢复,是我们写的变异就
# 恢复,**两者都不是就是有人在这期间改过它,不覆盖**,保留一切并报冲突。


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_source(paths, path, data):
    """原地写:保留 inode、属主、xattr、硬链接。

    写之前在日志里落一个 `writing` 标记,写完才删:原地写可能写到一半(被 SIGKILL、ENOSPC、
    RLIMIT_FSIZE —— APFS 是写时复制,原地覆盖也要新块),那时文件既不是原文也不是变异。
    有这个标记,恢复就知道那半截是**我们自己**写的,可以无条件写回原文;没有它,恢复只能
    当成「有人改过」而拒绝 —— 而且会一直拒绝下去。"""
    marker = os.path.join(paths.journal, "writing")
    with open(marker, "w") as m:
        m.flush()
        os.fsync(m.fileno())
    fsync_dir(paths.journal)
    with open(path, "r+b") as f:
        f.write(data)
        f.truncate()
        f.flush()
        os.fsync(f.fileno())
    os.unlink(marker)
    fsync_dir(paths.journal)


def read_journal(paths):
    """→ (src, original, meta) 或抛 Refused(日志损坏)。"""
    try:
        with open(os.path.join(paths.journal, "meta.json"), encoding="utf-8") as f:
            meta = json.load(f)
        with open(os.path.join(paths.journal, "bak"), "rb") as f:
            original = f.read()
    except (OSError, ValueError) as e:
        raise Refused(f"日志 {paths.journal} 读不全({e}) —— 不据它写任何东西;请人工检查后删掉它")
    if sha(original) != meta.get("original_sha"):
        raise Refused(f"日志 {paths.journal} 的备份与记录的 sha256 不符 —— 不据它写任何东西")
    return meta["path"], original, meta


def write_journal(paths, src, original, mutated, anchor="", repl=""):
    tmp = tempfile.mkdtemp(dir=paths.gitdir, prefix="mutate-inflight.tmp-")
    with open(os.path.join(tmp, "bak"), "wb") as f:
        f.write(original)
        f.flush()
        os.fsync(f.fileno())
    with open(os.path.join(tmp, "meta.json"), "w", encoding="utf-8") as f:
        json.dump({"path": src, "original_sha": sha(original), "mutated_sha": sha(mutated),
                   "anchor": anchor, "repl": repl}, f)
        f.flush()
        os.fsync(f.fileno())
    fsync_dir(tmp)
    os.rename(tmp, paths.journal)  # 已存在就抛 —— check_no_journal 已先拦过
    fsync_dir(paths.gitdir)


def drop_journal(paths):
    # 先 rename 成临时名字再删:rmtree 删到一半被打断,留下的是 check_no_journal 会清掉的
    # 临时目录,而不是一个缺了 meta.json、recover 又读不了的正式日志。
    doomed = paths.journal + ".tmp-drop"
    os.rename(paths.journal, doomed)
    fsync_dir(paths.gitdir)
    shutil.rmtree(doomed, ignore_errors=True)


def restore(paths, src, original, mutated_sha, meta=None):
    """把 src 恢复成 original 并逐字节核对;成功才删日志。返回 True/False。"""
    try:
        try:
            with open(src, "rb") as f:
                current = f.read()
        except FileNotFoundError:
            say(f"  ⛔ {src} 不存在了 —— 不替你重建它。原文备份在 {paths.journal}/bak;"
                f"核对后自行处理,再删掉 {paths.journal}")
            return False
        ours_half_written = os.path.exists(os.path.join(paths.journal, "writing"))
        if current != original:
            if sha(current) != mutated_sha and not ours_half_written:
                conflict_message(paths, src, current, meta)
                return False
            write_source(paths, src, original)
            with open(src, "rb") as f:
                if f.read() != original:
                    raise OSError("写回后逐字节比对不上")
    except OSError as e:
        say(f"  ⛔ 恢复 {src} 失败:{e}。原文备份保留在 {paths.journal}/bak —— "
            f"运行 `python3 docs/agent/mutate.py recover`")
        return False
    drop_journal(paths)
    return True


def conflict_message(paths, src, current, meta):
    """有人在这期间改过它:不覆盖 —— 但要说清楚**变异还在不在文件里**,否则人会把它一起提交。"""
    say(f"  ⛔ {src} 既不是原文、也不是我们写进去的变异 —— 有人在这期间改过它。不覆盖。")
    repl = (meta or {}).get("repl")
    text = current.decode("utf-8", errors="replace")
    if repl and repl in text:
        line = text[: text.index(repl)].count("\n") + 1
        say(f"  ⚠️ 变异**仍在文件里**(第 {line} 行附近):把 {repl!r} 改回 {meta.get('anchor')!r},"
            f"再保留你自己的修改")
    say(f"  原文备份在 {paths.journal}/bak(对比:diff {paths.journal}/bak {src});"
        f"处理完再删掉 {paths.journal}")


def pause(paths, phase):
    """自证用的相位栅栏:MUT_PAUSE_AT=<phase> 时在这里停住,直到收到信号或 continue 文件出现。"""
    if os.environ.get("MUT_PAUSE_AT") != phase:
        return
    marker = os.path.join(paths.gitdir, f"mutate-paused-{phase}")
    go = os.path.join(paths.gitdir, f"mutate-continue-{phase}")
    open(marker, "w").close()
    deadline = time.monotonic() + 60
    while not interrupted() and not os.path.exists(go) and time.monotonic() < deadline:
        time.sleep(0.02)
    for p in (marker, go):
        try:
            os.unlink(p)
        except OSError:
            pass


# ── 跑子进程 ─────────────────────────────────────────────────────────


def descendants(root):
    """root 的全部后代 pid(按 ppid 快照)。setsid 另起会话的后代不在 root 的进程组里,
    只有趁父进程还活着时按 ppid 才找得到。"""
    try:
        out = subprocess.run(["ps", "-A", "-o", "pid=,ppid="], capture_output=True, text=True).stdout
    except OSError:
        return []
    kids = {}
    for line in out.splitlines():
        try:
            pid, ppid = map(int, line.split())
        except ValueError:
            continue
        kids.setdefault(ppid, []).append(pid)
    found, todo = [], [root]
    while todo:
        for k in kids.get(todo.pop(), []):
            found.append(k)
            todo.append(k)
    return found


def supervise(cmd, cwd, timeout):
    """→ ("DONE", rc, text) / ("TIMEOUT",) / ("INTERRUPTED", signum)。不论怎么结束,整棵树都杀掉。"""
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace") as out:
        # 颜色转义会让行首锚定的判据全部失配(一个真崩溃被读成「不认识」)。
        env = dict(os.environ, CARGO_TERM_COLOR="never")
        proc = subprocess.Popen(
            cmd, cwd=cwd, env=env, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
            start_new_session=True,  # setsid 在 exec 之前:拿到 pid 时进程组已存在
        )
        deadline = time.monotonic() + timeout
        verdict = None
        while proc.poll() is None:
            if interrupted():
                verdict = ("INTERRUPTED", interrupted())
                break
            if time.monotonic() >= deadline:
                verdict = ("TIMEOUT",)
                break
            time.sleep(0.02)
        # 先按 ppid 快照后代(另起会话的也在内),再杀组、再逐个杀:测试进程可能比 cargo 活得久。
        stray = descendants(proc.pid)
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        for pid in stray:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        # 等它也要有上限:杀组失败(组不存在)时 cargo 可能还活着,无上限的 wait 会把
        # 「挂住」变成脚手架自己挂住 —— 元测试里真发生过,自证因此一声不吭。
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        if verdict:
            return verdict
        out.seek(0)
        return ("DONE", proc.returncode, out.read())


SUMMARY = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; \d+ ignored; \d+ measured; \d+ filtered out"
)
RUNNING = re.compile(r"^\s+Running unittests .* \((.+)\)\s*$")
# cargo 自己的顶层诊断,行首锚定 —— 测试输出里夹带的同样字样不算。
COMPILE_LINE = re.compile(
    r"^(error\[E\d+\]|error: could not compile |error: failed to run custom build command )"
)


def run_tests(paths, crate, filters, timeout, exe=None):
    """→ ("PASS", n, line) / ("FAIL", line) / ("CRASH", why) / ("COMPILE",) / ("TIMEOUT",) /
         ("UNKNOWN", why) / ("INTERRUPTED", signum)"""
    r = supervise([os.environ.get("MUT_CARGO", "cargo"), "test", "-p", crate, "--lib", *filters],
                  paths.rust, timeout)
    if r[0] != "DONE":
        return r
    return classify(r[2], r[1], exe)


def classify(text, rc, exe=None):
    lines = text.splitlines()
    summaries = [SUMMARY.match(l) for l in lines]
    summaries = [m for m in summaries if m]
    if len(summaries) > 1:
        return ("UNKNOWN", f"{len(summaries)} 行 result —— `--lib` 只该有一个测试二进制")
    if len(summaries) == 1:
        m = summaries[0]
        status, passed, failed = m.group(1), int(m.group(2)), int(m.group(3))
        if status == "ok" and failed == 0 and rc == 0:
            return ("PASS", passed, m.group(0))
        if status == "FAILED" and failed > 0 and rc != 0:
            return ("FAIL", m.group(0))
        return ("UNKNOWN", f"result 行与退出码矛盾(rc={rc}):{m.group(0)}")
    # 崩溃先判:只认「cargo 起的那一个 lib 测试二进制」没正常退出 —— 精确匹配到它,就不
    # 让测试输出里夹带的「could not compile」把一个被抓到的变异读成作废。
    # 那个二进制的路径:基线里从 compiler-artifact 记下的最可靠(`cargo -q` 不打印
    # `Running unittests` 行);没有才退回去读那一行。
    running = [RUNNING.match(l) for l in lines]
    running = [m.group(1) for m in running if m]
    if exe is None and len(running) == 1:
        exe = running[0]
    crashed = [l for l in lines if "process didn't exit successfully" in l]
    if exe and len(running) <= 1 and len(crashed) == 1 and rc != 0:
        c = crashed[0]
        # 路径要整段匹配:前面是反引号或 /,后面是反引号或空格(cargo 把测试参数接在路径后面,
        # `…/x-hash supervise`)。子串匹配会让 `…/x-1` 也认下 `…/x-12345`。
        if re.search(r"[`/]" + re.escape(exe.lstrip("./")) + r"[` ]", c):
            why = re.search(r"\(([^()]*)\)\s*$", c)
            return ("CRASH", why.group(1) if why else "非正常退出")
    if any(COMPILE_LINE.match(l) for l in lines):
        return ("COMPILE",)
    return ("UNKNOWN", "既没有 result 行,也认不出是编译失败或测试进程崩溃")


# ── 基线 ────────────────────────────────────────────────────────────


def parse_depinfo(text):
    """Make 格式 dep-info 的第一条规则里的依赖文件。处理续行与 `\\ ` / `\\#` / `\\\\` 转义。"""
    rule = text.split("\n\n", 1)[0].replace("\\\n", " ")
    # 找第一个未转义的 ": "
    i, target_end = 0, None
    while i < len(rule) - 1:
        if rule[i] == "\\":
            i += 2
            continue
        if rule[i] == ":" and rule[i + 1] in " \n":
            target_end = i
            break
        i += 1
    if target_end is None:
        return []
    deps, cur, i, body = [], [], 0, rule[target_end + 1:]
    while i < len(body):
        c = body[i]
        if c == "\\" and i + 1 < len(body) and body[i + 1] in " #\\":
            cur.append(body[i + 1])
            i += 2
            continue
        if c.isspace():
            if cur:
                deps.append("".join(cur))
                cur = []
        else:
            cur.append(c)
        i += 1
    if cur:
        deps.append("".join(cur))
    return deps


def lib_sources(paths, crate, filters):
    """`--lib` 测试真正编译进去的源文件(规范化绝对路径),以及那个测试二进制的路径。
    带上与跑测试**同样的参数**:`--no-default-features` 之类会改变编译范围,不带就会把一个
    实际没被编译的文件算进可变异清单(假 🟢)。"""
    r = supervise(
        [os.environ.get("MUT_CARGO_META", "cargo"), "test", "-p", crate, "--lib", "--no-run",
         "--message-format=json", *filters],
        paths.rust, float(os.environ.get("MUT_TIMEOUT", "150")),
    )
    if r[0] == "INTERRUPTED":
        return r
    if r[0] == "TIMEOUT":
        raise Refused(f"`cargo test -p {crate} --lib --no-run` 挂住 —— 无法确定哪些文件会被编译")
    if r[1] != 0:
        raise Refused(f"`cargo test -p {crate} --lib --no-run` 失败 —— crate 名不对或编译不过")
    exe = None
    for line in r[2].splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        target = m.get("target", {})
        if (m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test")
                and "lib" in target.get("kind", [])
                and target.get("name") == crate.replace("-", "_") and m.get("executable")):
            exe = m["executable"]
    if not exe or not os.path.exists(exe + ".d"):
        raise Refused(f"读不到 {crate} lib 测试的 dep-info —— 无法确定哪些文件会被编译")
    with open(exe + ".d", encoding="utf-8") as f:
        deps = parse_depinfo(f.read())
    return sorted({os.path.realpath(os.path.join(paths.rust, p)) for p in deps}), exe


def fingerprint(paths, files):
    """每个文件的 sha256。基线之后树变了,读数说的就不是基线那棵树 —— 包括一个恰好把
    某条红测试「修好」的变异被读成 🟢。"""
    out = {}
    for p in files:
        try:
            with open(p, "rb") as f:
                out[p] = sha(f.read())
        except OSError:
            out[p] = None
    return out


def watched_files(paths, sources):
    """rust/ 下 git 知道的全部文件(已跟踪 + 未跟踪未忽略)。只看被变异 crate 自己的源码不够:
    path 依赖(agent24-domain 之类)、workspace 的 Cargo.toml 变了,测试行为照样会变 —— 复审
    实测:基线之后改 agent24-domain,mut 照给 🟢。"""
    out = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z", "--", "rust"],
                         cwd=paths.root, capture_output=True).stdout
    listed = {os.path.realpath(os.path.join(paths.root, p.decode("utf-8", "replace")))
              for p in out.split(b"\0") if p}
    return sorted(listed | set(sources))


def cmd_baseline(paths, crate, filters):
    check_no_journal(paths)
    try:
        os.unlink(paths.baseline)  # 先清掉:一次失败的重立,不能让上一次的好基线继续生效
    except FileNotFoundError:
        pass
    found = lib_sources(paths, crate, filters)
    if found[0] == "INTERRUPTED":
        return 128 + found[1]
    sources, exe = found
    prints = fingerprint(paths, watched_files(paths, sources))
    r = run_tests(paths, crate, filters, float(os.environ.get("MUT_TIMEOUT", "150")))
    if r[0] == "INTERRUPTED":
        return 128 + r[1]
    if r[0] != "PASS":
        say(f"  ⛔ 基线不是全绿({' '.join(map(str, r))}) —— 停止,先修基线")
        return REFUSED
    if r[1] == 0:
        say(f"  ⛔ 基线跑了 0 个测试({r[2]}) —— 过滤词不对?停止")
        return REFUSED
    pause(paths, "baseline-after-run")
    if interrupted():
        return 128 + interrupted()
    if fingerprint(paths, prints.keys()) != prints:
        say("  ⛔ 立基线期间源码被改动了 —— 这个绿说的不是现在这棵树,重来")
        return REFUSED
    with open(paths.baseline, "w", encoding="utf-8") as f:
        json.dump({"root": paths.root, "crate": crate, "filters": filters, "exe": exe,
                   "passed": r[1], "sources": sources, "fingerprint": prints}, f)
    say(f"  {'基线(必须全绿,否则后面读数无意义)':<44} {r[2]}")
    return 0


# ── 一格变异 ─────────────────────────────────────────────────────────


def inject(original, anchor, repl):
    """→ (新内容 bytes, None) 或 (None, 拒绝原因)"""
    try:
        s = original.decode("utf-8")
    except UnicodeDecodeError:
        return None, "文件不是 UTF-8"
    if anchor not in s:
        return None, "锚点不存在"
    if s.count(anchor) > 1:
        return None, f"锚点出现 {s.count(anchor)} 次,不唯一"
    if anchor == repl:
        return None, "替换与锚点相同"
    if anchor in repl:
        return None, "纯插入:替换里仍完整包含锚点,原代码还在 → 行为未必变"
    out = s.replace(anchor, repl, 1)
    # 真正被改动的区间(去掉公共前后缀),而不是锚点所在行的行首:行尾注释
    # `x = 1; // retry 3 times` 里改 3 → 4,锚点所在行不以 // 开头,改的却只是注释。
    p = 0
    while p < min(len(s), len(out)) and s[p] == out[p]:
        p += 1
    line_start = s.rfind("\n", 0, p) + 1
    if "//" in s[line_start:p]:
        return None, "改动落在 // 注释里 —— 改注释不改行为(含行尾注释;字符串里的 // 也会被这样拒,方向是保守的)"
    return out.encode("utf-8"), None


FAILED_TEST = re.compile(r"^test (\S+) \.\.\. FAILED\b")


def failed_tests(r_text):
    return sorted(m.group(1) for m in (FAILED_TEST.match(l) for l in r_text.splitlines()) if m)


def run_red(paths, base, timeout):
    """跑一次,红的话连同失败的测试名一起返回:两次变异运行要红得一样,才不是偶发。"""
    r = supervise([os.environ.get("MUT_CARGO", "cargo"), "test", "-p", base["crate"], "--lib",
                   *base["filters"]], paths.rust, timeout)
    if r[0] != "DONE":
        return r, None
    return classify(r[2], r[1], base.get("exe")), failed_tests(r[2])


def cmd_mut(paths, file, anchor, repl, label):
    check_no_journal(paths)
    try:
        with open(paths.baseline, encoding="utf-8") as f:
            base = json.load(f)
    except (OSError, ValueError):
        raise Refused("没有可用的基线(先 mut_baseline,且必须全绿),拒绝开始")
    if base.get("root") != paths.root:
        raise Refused(f"基线是在另一棵树上立的({base.get('root')}),这里是 {paths.root}")
    src = os.path.realpath(os.path.join(paths.root, file))
    if not os.path.isfile(src):
        raise Refused(f"文件不存在: {file}")
    if src not in base["sources"]:
        raise Refused(f"{file} 不在 `{base['crate']}` 的 --lib 测试编译范围里 —— 改它读不到任何东西,读数会是假 🟢")
    prints = base.get("fingerprint", {})
    now = fingerprint(paths, prints.keys())
    if not prints or now != prints:
        changed = sorted(os.path.relpath(k, paths.root) for k in prints if now.get(k) != prints[k])
        raise Refused(f"基线之后源码变了({', '.join(changed[:3]) or '无指纹'}) —— 读数会说的是另一棵树,先重立基线")

    with open(src, "rb") as f:
        original = f.read()
    mutated, why = inject(original, anchor, repl)
    if mutated is None:
        say(f"  {label:<44} ⛔ 注入自证失败({why}),不读测试结果")
        return VOID

    timeout = float(os.environ.get("MUT_TIMEOUT", "150"))
    if interrupted():
        return 128 + interrupted()
    write_journal(paths, src, original, mutated, anchor, repl)
    pause(paths, "after-journal")
    r = r2 = names = names2 = None
    failure = None
    try:
        if not interrupted():
            write_source(paths, src, mutated)
            pause(paths, "after-inject")
        if not interrupted():
            r, names = run_red(paths, base, timeout)
            if r[0] in ("FAIL", "CRASH") and not interrupted():
                r2, names2 = run_red(paths, base, timeout)
            pause(paths, "after-run")
    except Exception as e:  # noqa: BLE001 — 先恢复,再决定退出码
        failure = e
    restored = restore(paths, src, original, sha(mutated),
                       {"anchor": anchor, "repl": repl})
    if not restored:
        return RESTORE_FAILED  # 源码没恢复:这条压过一切,包括内部错误和信号
    if failure is not None:
        raise failure
    if interrupted():
        say(f"\n  ⛔ 被信号 {interrupted()} 打断 —— 源码已恢复")
        return 128 + interrupted()
    if r is None:
        say(f"  {label:<44} ⛔ 变异没能写进源码,本格作废")
        return VOID
    for x in (r, r2):
        if x is not None and x[0] == "INTERRUPTED":
            return 128 + x[1]
    pause(paths, "before-verdict")
    if interrupted():
        return 128 + interrupted()
    if fingerprint(paths, prints.keys()) != prints:
        say(f"  {label:<44} ⛔ 跑测试期间别的源码被改了 —— 读数说的不是基线那棵树,本格作废")
        return VOID

    kind = r[0]
    if kind in ("FAIL", "CRASH"):
        # 两道确认:变异版再跑一次要红得一样(同一类、同一批失败的测试)—— 否则是偶发;
        # 恢复后的原样再跑一次要是基线 —— 否则是基线漂移。
        if r2 is None or r2[0] != kind or names2 != names:
            say(f"  {label:<44} ⛔ 变异版两次跑得不一样({r[0]} {names} / "
                f"{r2[0] if r2 else '-'} {names2}) —— 测试不稳定,本格作废")
            return VOID
        again = run_tests(paths, base["crate"], base["filters"], timeout, base.get("exe"))
        if again[0] == "INTERRUPTED":
            return 128 + again[1]
        if again[0] != "PASS" or again[1] != base["passed"]:
            say(f"  {label:<44} ⛔ 未变异时也不是基线({' '.join(map(str, again[:2]))}) —— 基线漂移或测试不稳定,本格作废")
            return VOID
        if kind == "FAIL":
            say(f"  {label:<44} 🔴 {r[1]}")
        else:
            say(f"  {label:<44} 🔴 测试进程崩溃({r[1]})")
        return RED
    if kind == "PASS":
        if r[1] != base["passed"]:
            say(f"  {label:<44} ⛔ 测试数与基线不一致({r[1]} ≠ 基线 {base['passed']}),本格作废")
            return VOID
        say(f"  {label:<44} 🟢 存活(判据不承重) {r[2]}")
        return ALIVE
    if kind == "COMPILE":
        say(f"  {label:<44} ⛔ 编译失败 —— 不是测试红,本格作废")
        return VOID
    if kind == "TIMEOUT":
        say(f"  {label:<44} ⏱ 挂住 —— 不是测试结果,去看它挂在哪")
        return TIMEOUT
    say(f"  {label:<44} ⛔ 无法识别的输出({r[1] if len(r) > 1 else ''}),本格作废")
    return VOID


def cmd_recover(paths):
    if not os.path.isdir(paths.journal):
        say("  没有未收尾的变异")
        return 0
    src, original, meta = read_journal(paths)
    real_root = os.path.realpath(paths.root) + os.sep
    if not os.path.realpath(src).startswith(real_root):
        raise Refused(f"日志指向 {src},不在这棵树({paths.root})里 —— 不写")
    if restore(paths, src, original, meta.get("mutated_sha"), meta):
        say(f"  ✓ 已恢复 {src}")
        return 0
    return RESTORE_FAILED


def main(argv):
    for s in SIGNALS:
        signal.signal(s, _on_signal)
    try:
        paths = Paths()
        cmd = argv[1] if len(argv) > 1 else ""
        if cmd == "recover":
            _lock = take_lock(paths)
            return cmd_recover(paths)
        if cmd == "baseline" and len(argv) >= 3:
            _lock = take_lock(paths)
            return cmd_baseline(paths, argv[2], argv[3:])
        if cmd == "mut" and len(argv) == 6:
            _lock = take_lock(paths)
            return cmd_mut(paths, *argv[2:6])
        say(__doc__.split("\n\n")[0])
        return REFUSED
    except Refused as e:
        say(f"  ⛔ {e}")
        return REFUSED
    except Exception as e:  # noqa: BLE001 — 源码已由 cmd_mut 的 finally 恢复;这里只区分退出码
        say(f"  ⛔ 脚手架内部错误:{type(e).__name__}: {e}")
        return INTERNAL


def exit_code(rc):
    """最后一次检查之后才到的信号,也不能被一个普通读数盖过去 —— 但「源码没恢复」(2)
    不许被任何东西盖过去:128+n 的含义是「源码已恢复」。"""
    if interrupted() and rc < 128 and rc != RESTORE_FAILED:
        return 128 + interrupted()
    return rc


if __name__ == "__main__":
    sys.exit(exit_code(main(sys.argv)))
