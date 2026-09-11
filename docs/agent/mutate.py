#!/usr/bin/env python3
"""变异测试的执行核心 —— `mutate.sh` 是它的薄包装，平时用那个。

    python3 docs/agent/mutate.py baseline <crate> [测试过滤…]
    python3 docs/agent/mutate.py mut <文件> <锚点> <替换> <标签>
    python3 docs/agent/mutate.py recover        # 上次被 SIGKILL 之类打断时,恢复被变异的文件

退出码(`mutate.sh` 据此设 `$_MUT_LAST`):
    0 RED   变异被抓到          10 ALIVE  存活(判据不承重)
    20 VOID 读数作废            30 TIMEOUT 挂住
    1  拒绝开始(前置条件不满足)  2  源码恢复失败 —— 备份保留在 git-dir 里,见提示
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
  - 源码的备份先落进 git-dir 下的**日志**,再动源码;写源码与恢复都是同目录临时文件 +
    `os.replace`(原子),恢复后逐字节比对,比对不上就保留日志、报错,绝不删唯一的备份。
    被 SIGKILL 打断后日志还在,下一次 baseline/mut 会拒绝开始并指向 `recover`。
  - 一把 `flock` 锁:同一棵树上两个变异不能同时跑(第二个会把第一个的变异当成原文备份,
    最后写回去)。
  - **能改的文件只限于 `--lib` 测试真正编译进去的那些** —— 从 cargo 的 dep-info 读出来,
    按规范化路径比。`src/bin/`、`tests/`、`build.rs`、没有 `mod` 声明的文件、`../` 与
    软链,都不在这份清单里。(前缀比较能被 `crate/../别处` 绕过,复审实测过。)
  - 🔴 不是一次读数说了算:恢复源码后**再跑一次未变异的版本**,必须仍是全绿且测试数等于
    基线,这次红才归到变异头上。否则是基线漂移或测试不稳定 —— 作废。

── 它拦的假结论(每一条都在实际复审里发生过) ─────────────────────────

  1. 锚点不存在 / 不唯一 / 替换里仍完整包含锚点(纯插入)/ 锚点所在行是 `//` 注释 → 作废。
     纯插入那条也会拒绝 `x → !(x)` 这种包裹式变异,方向是保守的;换一个锚点即可。
  2. 编译失败(含 build script 失败)→ 作废。编译错误不是测试结果。
  3. 测试数与基线不一致(过滤词打错 → 0 个测试;`#[test]` 被变异藏掉)→ 作废。
     基线必须 > 0 个测试且全绿。
  4. 输出里 result 行不是恰好一行、或与退出码矛盾 → 作废,不落到 🔴 或 🟢。
  5. 测试进程崩溃(abort/段错误)算 🔴,但只在输出能确定崩的就是那个 lib 测试二进制时;
     别的子进程(build script、包装器)失败 → 作废。

**仍然拦不住的**,如实写:唯一锚点落在 `#[cfg(...)]` 编译掉的代码、字符串字面量、或块注释
里 → 假 🟢;测试自己用 setsid 另起的进程组(os-proto 的模块进程就是)不在我们杀的那一组
里。选锚点时自己确认它是被编译、被执行的代码。

── 一个试过并否定的做法 ────────────────────────────────────────────────

曾想用「编译产物哈希是否变化」识别空操作。**实测不成立**:加一个没人用的 `const` 同样改变
哈希;同一份源码两次构建的哈希也不同。哈希能证明「有变化」,证明不了「行为有变化」。
"""

import errno
import fcntl
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
REFUSED, RESTORE_FAILED = 1, 2

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
    if os.path.isdir(paths.journal):
        target = ""
        try:
            with open(os.path.join(paths.journal, "path"), encoding="utf-8") as f:
                target = f.read()
        except OSError:
            pass
        raise Refused(
            f"上一次变异没有收尾(被 SIGKILL 或恢复失败):{target or '<未知文件>'} 可能仍处于被变异状态。"
            f"先运行 `python3 docs/agent/mutate.py recover`"
        )


# ── 原子写与恢复 ─────────────────────────────────────────────────────


def atomic_write(path, data):
    d = os.path.dirname(path)
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".mutate-")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        shutil.copymode(path, tmp)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def write_journal(paths, src, original):
    os.mkdir(paths.journal)  # 已存在就抛 —— check_no_journal 已先拦过
    with open(os.path.join(paths.journal, "bak"), "wb") as f:
        f.write(original)
        f.flush()
        os.fsync(f.fileno())
    with open(os.path.join(paths.journal, "path"), "w", encoding="utf-8") as f:
        f.write(src)
        f.flush()
        os.fsync(f.fileno())


def restore(paths, src, original):
    """恢复并逐字节核对;成功才删日志。返回 True/False。"""
    try:
        atomic_write(src, original)
        with open(src, "rb") as f:
            ok = f.read() == original
    except OSError as e:
        say(f"  ⛔ 恢复 {src} 失败:{e}")
        ok = False
    if ok:
        shutil.rmtree(paths.journal, ignore_errors=True)
    else:
        say(f"  ⛔ 源码没有恢复成原样。备份保留在 {paths.journal}/bak —— "
            f"运行 `python3 docs/agent/mutate.py recover`")
    return ok


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


# ── 跑测试与读结果 ────────────────────────────────────────────────────

SUMMARY = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; \d+ ignored; \d+ measured; \d+ filtered out"
)
RUNNING = re.compile(r"^\s+Running unittests .* \((.+)\)\s*$")


def run_tests(paths, crate, filters, timeout):
    """→ ("PASS", n, line) / ("FAIL", line) / ("CRASH", why) / ("COMPILE",) / ("TIMEOUT",) /
         ("UNKNOWN", why) / ("INTERRUPTED", signum)"""
    cargo = os.environ.get("MUT_CARGO", "cargo")
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace") as out:
        proc = subprocess.Popen(
            [cargo, "test", "-p", crate, "--lib", *filters],
            cwd=paths.rust,
            stdout=out,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
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
        # 不论怎么结束,整组杀一遍:测试进程可能比 cargo 活得久。
        try:
            os.killpg(proc.pid, signal.SIGKILL)
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
        text = out.read()
    return classify(text, proc.returncode)


def classify(text, rc):
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
    if any("failed to run custom build command" in l for l in lines):
        return ("COMPILE",)
    if any(re.match(r"^error\[E\d+\]", l) or "could not compile" in l for l in lines):
        return ("COMPILE",)
    # 崩溃:只认「cargo 起的那一个 lib 测试二进制」没正常退出。
    running = [RUNNING.match(l) for l in lines]
    running = [m.group(1) for m in running if m]
    crashed = [l for l in lines if "process didn't exit successfully" in l]
    if len(running) == 1 and len(crashed) == 1 and rc != 0:
        exe = running[0]
        c = crashed[0]
        if f"`{exe}" in c or f"`{os.path.join('.', exe)}" in c or exe in c:
            why = re.search(r"\(([^()]*)\)\s*$", c)
            return ("CRASH", why.group(1) if why else "非正常退出")
    return ("UNKNOWN", "既没有 result 行,也认不出是编译失败或测试进程崩溃")


# ── 基线 ────────────────────────────────────────────────────────────


def lib_sources(paths, crate):
    """`--lib` 测试真正编译进去的源文件(规范化绝对路径)。"""
    proc = subprocess.run(
        ["cargo", "test", "-p", crate, "--lib", "--no-run", "--message-format=json"],
        cwd=paths.rust, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        raise Refused(f"`cargo test -p {crate} --lib --no-run` 失败 —— crate 名不对或编译不过")
    exe = None
    for line in proc.stdout.splitlines():
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
        first = f.readline()
    _, _, deps = first.partition(": ")
    files = re.split(r"(?<!\\) ", deps.strip())
    out = set()
    for p in files:
        p = p.replace("\\ ", " ")
        if not p:
            continue
        out.add(os.path.realpath(os.path.join(paths.rust, p)))
    return sorted(out)


def cmd_baseline(paths, crate, filters):
    check_no_journal(paths)
    try:
        os.unlink(paths.baseline)  # 先清掉:一次失败的重立,不能让上一次的好基线继续生效
    except FileNotFoundError:
        pass
    sources = lib_sources(paths, crate)
    r = run_tests(paths, crate, filters, float(os.environ.get("MUT_TIMEOUT", "150")))
    if r[0] == "INTERRUPTED":
        return 128 + r[1]
    if r[0] != "PASS":
        say(f"  ⛔ 基线不是全绿({' '.join(map(str, r))}) —— 停止,先修基线")
        return REFUSED
    if r[1] == 0:
        say(f"  ⛔ 基线跑了 0 个测试({r[2]}) —— 过滤词不对?停止")
        return REFUSED
    with open(paths.baseline, "w", encoding="utf-8") as f:
        json.dump({"root": paths.root, "crate": crate, "filters": filters,
                   "passed": r[1], "sources": sources}, f)
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
    i = s.index(anchor)
    line_start = s.rfind("\n", 0, i) + 1
    line_end = s.find("\n", i)
    line = s[line_start: line_end if line_end != -1 else len(s)]
    if line.lstrip().startswith("//"):
        return None, "锚点所在行是 // 注释 —— 改注释不改行为"
    return s.replace(anchor, repl, 1).encode("utf-8"), None


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

    with open(src, "rb") as f:
        original = f.read()
    mutated, why = inject(original, anchor, repl)
    if mutated is None:
        say(f"  {label:<44} ⛔ 注入自证失败({why}),不读测试结果")
        return VOID

    timeout = float(os.environ.get("MUT_TIMEOUT", "150"))
    if interrupted():
        return 128 + interrupted()
    write_journal(paths, src, original)
    pause(paths, "after-journal")
    status = None
    r = None
    try:
        if not interrupted():
            atomic_write(src, mutated)
            pause(paths, "after-inject")
        if not interrupted():
            r = run_tests(paths, base["crate"], base["filters"], timeout)
            pause(paths, "after-run")
    finally:
        restored = restore(paths, src, original)
    if not restored:
        return RESTORE_FAILED
    if interrupted():
        say(f"\n  ⛔ 被信号 {interrupted()} 打断 —— 源码已恢复")
        return 128 + interrupted()
    if r is None:
        say(f"  {label:<44} ⛔ 变异没能写进源码,本格作废")
        return VOID

    kind = r[0]
    if kind in ("FAIL", "CRASH"):
        # 这次红是不是变异造成的:在恢复后的原样上再跑一次,必须还是那个全绿基线。
        again = run_tests(paths, base["crate"], base["filters"], timeout)
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
    with open(os.path.join(paths.journal, "path"), encoding="utf-8") as f:
        src = f.read()
    with open(os.path.join(paths.journal, "bak"), "rb") as f:
        original = f.read()
    if restore(paths, src, original):
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


if __name__ == "__main__":
    sys.exit(main(sys.argv))
