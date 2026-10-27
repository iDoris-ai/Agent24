#!/usr/bin/env python3
"""Knowledge (ab/weknora) first release: acceptance on a real machine.

    python3 -I scripts/kb-r1-accept.py --old <main bin dir> --new <release bin dir> [--work <dir>]

Cases: docs/agent/KB-R1-ACCEPTANCE.md. Each bin dir holds `agent24d`,
`agent24` and `agent24-documents` (`cargo build -p agent24d -p agent24-cli
-p agent24-documents`). Every daemon and CLI call
runs with its own AGENT24_DATA_DIR and a HOME under --work; ~/.agent24 must
stay untouched (Z1). No model is needed. Prints PASS/FAIL per check, writes
report.md in the work directory and exits non-zero on any FAIL.
"""

import argparse
import http.client
import json
import os
import secrets
import select
import shutil
import signal
import stat
import subprocess
import sys
import time

READY_TIMEOUT = 30
# How long the documents module may take to finish its handshake once the
# daemon is up (docs/documenting/acceptance/R6-RELEASE-USECASES.md, P3).
MODULE_READY_TIMEOUT = 30
BINDING = "kb-accept"
REPO = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))


def parse_args():
    p = argparse.ArgumentParser(add_help=True)
    p.add_argument("--old", required=True)
    p.add_argument("--new", required=True)
    p.add_argument("--work")
    return p.parse_args()


ARGS = parse_args()
OLD = os.path.realpath(ARGS.old)
NEW = os.path.realpath(ARGS.new)
for d in (OLD, NEW):
    for b in ("agent24d", "agent24"):
        if not os.access(os.path.join(d, b), os.X_OK):
            sys.exit(f"missing binary: {os.path.join(d, b)}")
REAL = os.path.realpath(os.path.join(os.environ.get("HOME", "/nonexistent"), ".agent24"))
WORK = os.path.realpath(ARGS.work or os.path.join(os.environ.get("TMPDIR", "/tmp"), f"kb-r1-accept.{os.getpid()}"))
if WORK == REAL or WORK.startswith(REAL + os.sep):
    sys.exit(f"--work must not be inside {REAL}")
# reap() tells this run's daemons by their working directory as lsof prints
# it; lsof escapes other bytes (and, under the C locale, any non-ASCII), so
# such a path would never match and a leftover daemon would be spared.
if not all(" " <= c <= "~" for c in WORK):
    sys.exit(f"--work must be a printable ASCII path: {WORK!r}")
if os.path.exists(WORK) and os.listdir(WORK):
    sys.exit(f"--work must be new or empty: {WORK}")
# reap() takes every daemon running under WORK for this run's, so a run nested
# inside another run's work dir would have its daemons killed by the outer one.
MARK = ".kb-r1-accept"
up = os.path.dirname(WORK)
while True:
    if os.path.exists(os.path.join(up, MARK)):
        sys.exit(f"--work must not be inside another run's work dir: {up}")
    if up == os.path.dirname(up):
        break
    up = os.path.dirname(up)
os.makedirs(WORK, exist_ok=True)
open(os.path.join(WORK, MARK), "w").close()
HOME = os.path.join(WORK, "home")
os.makedirs(HOME, exist_ok=True)

# Only what each case sets reaches a daemon or the CLI: every A24_* and
# AGENT24* variable goes (AGENT24D_BIN too, which would swap the daemon the
# CLI starts), and HOME points under --work.
BASE_ENV = {k: v for k, v in os.environ.items() if not k.startswith(("A24_", "AGENT24"))}
BASE_ENV["HOME"] = HOME

RESULTS = []
FAILS = 0


def check(cid, what, ok, evidence):
    global FAILS
    mark = "PASS" if ok else "FAIL"
    if not ok:
        FAILS += 1
    RESULTS.append(f"| {cid} | {what} | {mark} | {evidence} |")
    print(f"{mark} {cid} — {what} — {evidence}", flush=True)


def snapshot(top):
    """`top` itself (absent, or its mtime) and every path under it with its
    size and mtime: a directory created empty shows up too."""
    try:
        out = {top: ("dir", os.lstat(top).st_mtime_ns)}
    except FileNotFoundError:
        out = {top: ("absent", 0)}
    for root, dirs, files in os.walk(top):
        for name in dirs + files:
            path = os.path.join(root, name)
            try:
                st = os.lstat(path)
            except FileNotFoundError:
                continue
            out[path] = (st.st_size, st.st_mtime_ns)
    return out


def daemons():
    """Pids of `agent24d serve` processes from either bin dir."""
    out = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
    found = set()
    for line in out.splitlines():
        pid, _, cmd = line.strip().partition(" ")
        if any(cmd.startswith(os.path.join(d, "agent24d") + " serve") for d in (OLD, NEW)):
            found.add(int(pid))
    return found


BEFORE = daemons()


def strays():
    """Pids of processes run from a binary under --work: the documents
    module that `os install` put there and its daemon started."""
    out = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
    found = set()
    for line in out.splitlines():
        pid, _, cmd = line.strip().partition(" ")
        if cmd.startswith(WORK + os.sep):
            found.add(int(pid))
    return found


def cwd_of(pid):
    """A process's working directory, or None if it cannot be read. Every
    daemon this run starts runs in its own data dir under --work: the script
    starts it there, or starts the CLI that spawns it there (neither chdirs)."""
    try:
        if sys.platform.startswith("linux"):
            return os.readlink(f"/proc/{pid}/cwd")
        out = subprocess.run(["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"],
                             capture_output=True, text=True).stdout
        for line in out.splitlines():
            if line.startswith("n"):
                return line[1:]
    except OSError:
        pass
    return None


def reap():
    """Kill every daemon this run started and left behind. A CLI start that
    gave up waiting leaves its daemon running, maybe with no state file yet,
    so no pid is taken from a state file. Only a daemon that runs in a
    directory under --work is this run's: one another run or the user started from the
    same bin dir is left alone, as is one whose directory cannot be read."""
    for pid in daemons() - BEFORE:
        cwd = cwd_of(pid)
        if cwd is None or not os.path.realpath(cwd).startswith(WORK + os.sep):
            print(f"a daemon from the same bin dir is not this run's (pid {pid}, cwd {cwd!r}); "
                  "leaving it", file=sys.stderr)
            continue
        print(f"a daemon outlived its case (pid {pid}); killing it", file=sys.stderr)
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    for pid in strays():
        print(f"a module outlived its daemon (pid {pid}); killing it", file=sys.stderr)
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass


def env_for(data):
    env = dict(BASE_ENV)
    env["AGENT24_DATA_DIR"] = data
    env["A24_OS_PACKAGES"] = data + ".pk"
    return env


def call(port, method, path, token=None, body=None):
    """(status, json or None, raw text). Host is 127.0.0.1:port, as the daemon's
    Host guard expects. Never raises: no answer, or a broken one, is status 0."""
    headers = {}
    data = None
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    try:
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        conn.request(method, path, body=data, headers=headers)
        res = conn.getresponse()
        raw = res.read().decode("utf-8", "replace")
        conn.close()
    except (OSError, http.client.HTTPException) as err:
        return 0, None, f"no answer: {err!r}"
    try:
        parsed = json.loads(raw) if raw else None
    except ValueError:
        parsed = None
    return res.status, parsed, raw


def code_of(parsed):
    if isinstance(parsed, dict) and isinstance(parsed.get("error"), dict):
        return parsed["error"].get("code")
    return None


def cli(bin_dir, data, *args, extra_env=None):
    """`agent24 ARGS` on DATA: (exit code, what it said)."""
    env = env_for(data)
    env.update(extra_env or {})
    os.makedirs(data, exist_ok=True)
    try:
        out = subprocess.run([os.path.join(bin_dir, "agent24"), *args], env=env, cwd=data,
                             capture_output=True, timeout=90)
    except subprocess.TimeoutExpired:
        return None, "timed out"
    return out.returncode, (out.stdout + out.stderr).decode("utf-8", "replace").strip()


def read_line(proc, timeout):
    line = b""
    deadline = time.monotonic() + timeout
    while not line.endswith(b"\n") and time.monotonic() < deadline:
        r, _, _ = select.select([proc.stdout], [], [], 0.5)
        if r:
            chunk = os.read(proc.stdout.fileno(), 65536)
            if not chunk:
                break
            line += chunk
        elif proc.poll() is not None:
            break
    try:
        return json.loads(line) if line.strip() else None
    except ValueError:
        return None


class SpawnedDaemon:
    """agent24d started the way a desktop starts it, reading its ready line.

    `capabilities=True` is this release's desktop on macOS and Linux
    (apps/desktop/src/main/backend-manager.ts): the ready record on a private
    stdout pipe, stdin held open as the parent lease. `False` is main's
    desktop: `serve --port 0`, legacy, stdin ignored."""

    def __init__(self, bin_dir, data, capabilities=True):
        os.makedirs(data, exist_ok=True)
        self.data = data
        self.log_path = data + ".serve.log"
        self.log = open(self.log_path, "wb")
        args = [os.path.join(bin_dir, "agent24d"), "serve", "--port", "0"]
        if capabilities:
            args += ["--auth-mode", "capabilities", "--host-bootstrap-stdio"]
        self.proc = subprocess.Popen(
            args, stdin=subprocess.PIPE if capabilities else subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=self.log, env=env_for(data), cwd=data)
        self.ready = read_line(self.proc, READY_TIMEOUT)
        r = self.ready if isinstance(self.ready, dict) else {}
        self.port = r.get("port")
        self.token = r.get("product_host_token") if capabilities else r.get("token")

    def alive(self):
        return self.proc.poll() is None

    def stop(self):
        # stdin EOF ends a capability daemon's parent lease; a legacy one is
        # stopped by signal.
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
            else:
                self.proc.terminate()
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.log.close()


class CliDaemon:
    """`agent24 daemon start` as a user runs it: legacy mode."""

    def __init__(self, bin_dir, data):
        os.makedirs(data, exist_ok=True)
        self.bin = bin_dir
        self.data = data
        code, self.words = cli(bin_dir, data, "daemon", "start")
        self.port = self.token = None
        state = os.path.join(data, "daemon.json")
        if code == 0 and os.path.exists(state):
            with open(state) as f:
                s = json.load(f)
            self.port, self.token = s.get("port"), s.get("token")

    def stop(self):
        cli(self.bin, self.data, "daemon", "stop")


def install_documents(bin_dir, data):
    """The documents package assembled as its README says (domain-os.yml and
    the module binary; W6 needs no read engine), installed with `agent24 os
    install` before the daemon starts: (exit code, what it said)."""
    pkg = data + ".docpkg"
    os.makedirs(os.path.join(pkg, "bin"))
    shutil.copy(os.path.join(REPO, "rust", "apps", "agent24-documents", "domain-os.yml"), pkg)
    shutil.copy(os.path.join(bin_dir, "agent24-documents"), os.path.join(pkg, "bin"))
    return cli(bin_dir, data, "os", "install", pkg)


def documents_request(bin_dir, data, daemon, request, token=None):
    """`agent24 documents request --host-stdin --json` the desktop's way
    (apps/desktop/src/main/documents-cli.ts): the daemon's address, the token
    and the request on stdin, none of them in argv. The token is the
    daemon's own unless one is given. (exit code, the last stdout line as
    JSON or None.)"""
    body = {"base": f"http://127.0.0.1:{daemon.port}",
            "token": daemon.token if token is None else token, "request": request}
    try:
        out = subprocess.run([os.path.join(bin_dir, "agent24"), "documents", "request", "--host-stdin", "--json"],
                             input=json.dumps(body).encode(), env=env_for(data), cwd=data,
                             capture_output=True, timeout=90)
    except subprocess.TimeoutExpired:
        return None, None
    lines = out.stdout.decode("utf-8", "replace").strip().splitlines()
    try:
        return out.returncode, json.loads(lines[-1]) if lines else None
    except ValueError:
        return out.returncode, None


def result_of(answer):
    """(ok, status, error code) of the CLI's `result` line; Nones otherwise."""
    if not isinstance(answer, dict) or answer.get("type") != "result":
        return None, None, None
    return answer.get("ok"), answer.get("status"), code_of(answer)


def documents_ready(bin_dir, data, daemon):
    """Asks for `capabilities` until the module has finished its handshake
    (`module_not_ready` until then) or MODULE_READY_TIMEOUT has passed.
    (exit code, answer, seconds waited.)"""
    start = time.monotonic()
    while True:
        code, answer = documents_request(bin_dir, data, daemon, {"op": "capabilities"})
        waited = time.monotonic() - start
        if code == 0 or waited > MODULE_READY_TIMEOUT:
            return code, answer, waited
        time.sleep(0.3)


def start_run(port, token, binding=None):
    body = {"prompt": "kb-r1 accept"}
    if binding is not None:
        body["knowledge_binding_id"] = binding
    return call(port, "POST", "/api/v1/runs", token, body)


def host_change(port, token, method, seq, suffix="", body=None):
    return call(port, method, f"/api/v1/host/knowledge/bindings/{BINDING}{suffix}?seq={seq}", token, body)


def attach(port, token, name):
    manifest = (f"name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n"
                f"event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n"
                "impl_kind: attached_process\nkernel_capabilities: [events]\n")
    return call(port, "POST", "/api/v1/attached", token, {"manifest": manifest})


def files_containing(top, extra, needle):
    """(files that contain `needle`, files that could not be read). Only
    regular files are read. A file that cannot be read is not a miss: the
    check that asked must fail."""
    hits, unread = [], []
    paths = list(extra)
    for root, _, files in os.walk(top, onerror=lambda e: unread.append(str(e.filename))):
        paths += [os.path.join(root, f) for f in files]
    for path in paths:
        try:
            # Sockets and FIFOs hold no bytes to leak; opening one would fail.
            # stat, not lstat: a symlink to a regular file is read through, so
            # a secret behind a link (perhaps outside the data dir) is found.
            if not stat.S_ISREG(os.stat(path).st_mode):
                continue
            with open(path, "rb") as f:
                if needle.encode() in f.read():
                    hits.append(os.path.relpath(path, WORK))
        except FileNotFoundError:
            pass  # gone between listing and reading: nothing left to leak
        except OSError:
            unread.append(os.path.relpath(path, WORK))
    return hits, unread


def leak_check(cid, what, data, log, needle, ok=True, before=""):
    """Run once the daemon has stopped, so what it writes on the way out is
    scanned too."""
    hits, unread = files_containing(data, [log], needle)
    check(cid, what, ok and not hits and not unread,
          f"{before}停机后扫描：落盘 {hits or '无'}，读不了的文件 {unread or '无'}")


def binding_of(run):
    """(present, value) of input.knowledge_binding_id: the key is required."""
    inp = run.get("input") if isinstance(run, dict) else None
    if not isinstance(inp, dict) or "knowledge_binding_id" not in inp:
        return False, None
    return True, inp["knowledge_binding_id"]


Z1_BEFORE = snapshot(REAL)


def case_w1():
    host = None
    try:
        host = SpawnedDaemon(NEW, os.path.join(WORK, "w1"))
        r = host.ready if isinstance(host.ready, dict) else {}
        check("W1a", "宿主以 capabilities 模式拉起 daemon", r.get("auth_mode") == "capabilities"
              and isinstance(host.token, str) and isinstance(host.port, int),
              f"ready 行 auth_mode={r.get('auth_mode')!r}，port={host.port!r}，"
              f"product_host_token {'有' if host.token else '无'}")
        if not host.port:
            return
        s1, _, _ = call(host.port, "GET", "/api/v1/models", host.token)
        s2, p2, _ = call(host.port, "GET", "/api/v1/models")
        check("W1b", "宿主凭据能调 API，无凭据被拒", s1 == 200 and s2 == 401,
              f"带凭据 {s1}，不带 {s2} {code_of(p2)}")
    finally:
        if host:
            host.stop()
    if host and host.token:
        leak_check("W1c", "宿主凭据只在 ready 管道里，不落盘、不进日志", host.data, host.log_path, host.token)


def case_w2():
    host = None
    secret, answers, pushed = None, [], None
    try:
        host = SpawnedDaemon(NEW, os.path.join(WORK, "w2"))
        if not host.port:
            check("W2", "宿主 daemon 就绪", False, "没有 ready 行")
            return
        p, t = host.port, host.token
        s, run, _ = start_run(p, t)
        present, value = binding_of(run)
        check("W2a", "不指名 binding 的 run 照常建立，记为 null", s == 202 and present and value is None,
              f"{s}，input.knowledge_binding_id {'=' + repr(value) if present else '缺这个键'}")
        s, body, _ = start_run(p, t, BINDING)
        check("W2b", "指名没激活的 binding 被拒", s == 409 and code_of(body) == "knowledge_binding_inactive",
              f"{s} {code_of(body)}")
        s1, _, _ = host_change(p, t, "PUT", 1)
        s2, run, _ = start_run(p, t, BINDING)
        rid = run.get("id") if isinstance(run, dict) else None
        s3, shown, _ = call(p, "GET", f"/api/v1/runs/{rid}", t) if rid else (0, None, "")
        _, named = binding_of(shown)
        check("W2c", "激活后 run 能指名它，run 记着这个 binding", s1 == 204 and s2 == 202 and s3 == 200 and named == BINDING,
              f"激活 {s1}，建 run {s2}，读回 {s3} knowledge_binding_id={named!r}")
        s, body, _ = host_change(p, t, "PUT", 1)
        check("W2d", "过期的序号不能覆盖新的变更", s == 409 and code_of(body) == "stale_sequence",
              f"{s} {code_of(body)}")
        secret = "kbr1-" + secrets.token_hex(16)
        # Every answer from the push on is checked for the secret (W2e).
        s1, _, raw = host_change(p, t, "PUT", 2, "/credential",
                                 {"kind": "bearer", "value": secret, "expires_at": int(time.time()) + 600})
        answers.append(raw)
        s2, _, raw = start_run(p, t, BINDING)
        answers.append(raw)
        pushed = (s1, s2)
        s1, _, raw = host_change(p, t, "DELETE", 3)
        answers.append(raw)
        s2, body, raw = start_run(p, t, BINDING)
        answers.append(raw)
        check("W2f", "停用后再指名被拒", s1 == 204 and s2 == 409 and code_of(body) == "knowledge_binding_inactive",
              f"停用 {s1}，建 run {s2} {code_of(body)}")
    finally:
        if host:
            host.stop()
    if secret is not None:
        in_answers = any(secret in a for a in answers)
        leak_check("W2e", "推送凭据后，所有应答、数据目录和日志里都没有它", host.data, host.log_path, secret,
                   ok=pushed == (204, 202) and not in_answers,
                   before=f"推送 {pushed[0] if pushed else None}，再建 run {pushed[1] if pushed else None}，"
                          f"{len(answers)} 个应答里{'有' if in_answers else '无'}；")
    # Reverse control: main's daemon has no host interface and no check.
    old = None
    try:
        old = SpawnedDaemon(OLD, os.path.join(WORK, "w2-old"))
        if not old.port:
            check("W2r", "反面对照：main 的宿主 daemon 就绪", False, "没有 ready 行")
            return
        s1, _, _ = host_change(old.port, old.token, "PUT", 1)
        s2, _, _ = start_run(old.port, old.token, BINDING)
        check("W2r", "反面对照：main 没有宿主接口，也不查 binding", s1 != 204 and s2 == 202,
              f"main 激活 {s1}，指名没激活的 binding 建 run {s2}")
    finally:
        if old:
            old.stop()


def case_w3_w5():
    """W3 and W5 share one CLI-started daemon per side."""
    new = None
    try:
        new = CliDaemon(NEW, os.path.join(WORK, "w3"))
        if not new.port:
            check("W3", "`agent24 daemon start` 起来", False, new.words)
        else:
            p, t = new.port, new.token
            s, body, _ = start_run(p, t, BINDING)
            check("W3a", "普通 CLI 用户指名 binding 被拒", s == 403 and code_of(body) == "host_authority_required",
                  f"{s} {code_of(body)}")
            s, body, _ = host_change(p, t, "PUT", 1)
            check("W3b", "普通 CLI 用户调宿主接口被拒", s == 403 and code_of(body) == "host_authority_required",
                  f"{s} {code_of(body)}")
            s, _, _ = start_run(p, t)
            check("W3c", "不指名 binding 的 run 照常建立", s == 202, f"{s}")
            s1, b1, _ = attach(p, t, "host")
            s2, _, _ = attach(p, t, "kbacceptmod")
            msg = b1.get("error", {}).get("message", "") if isinstance(b1, dict) else ""
            check("W5a", "名为 host 的模块注册被拒，别的名字照常",
                  s1 == 400 and code_of(b1) == "invalid_manifest" and "reserved" in msg and s2 == 201,
                  f"host {s1} {code_of(b1)}，kbacceptmod {s2}")
    finally:
        if new:
            new.stop()
    old = None
    try:
        old = CliDaemon(OLD, os.path.join(WORK, "w3-old"))
        if not old.port:
            check("W3r", "反面对照：main 的 `agent24 daemon start`", False, old.words)
            return
        s, _, _ = start_run(old.port, old.token, BINDING)
        check("W3r", "反面对照：main 不查 binding，照常建 run", s == 202, f"{s}")
        s, _, _ = attach(old.port, old.token, "host")
        check("W5r", "反面对照：main 接受名为 host 的模块", s == 201, f"{s}")
    finally:
        if old:
            old.stop()


def case_w4():
    """The CLI next to a desktop-managed daemon on the same data dir."""
    host = None
    try:
        data = os.path.join(WORK, "w4")
        host = SpawnedDaemon(NEW, data)
        if not host.port:
            check("W4", "宿主 daemon 就绪", False, "没有 ready 行")
            return
        code, said = cli(NEW, data, "daemon", "status")
        check("W4a", "`agent24 daemon status` 认出它在跑，但拿不到关停报告",
              code == 0 and f"pid {host.proc.pid}" in said and "host authority unavailable" in said,
              f"退出 {code}：{said.splitlines()[0] if said else ''} …")
        code, said = cli(NEW, data, "models")
        check("W4b", "`agent24 models` 不能借用桌面的 daemon", code not in (0, None)
              and "host authority unavailable" in said and host.alive(), f"退出 {code}：{said[-80:]}")
        code, said = cli(NEW, data, "daemon", "stop")
        s, _, _ = call(host.port, "GET", "/api/v1/models", host.token)
        check("W4c", "`agent24 daemon stop` 停不掉桌面的 daemon", code not in (0, None)
              and "host authority unavailable" in said and host.alive() and s == 200,
              f"退出 {code}：{said[-80:]}；daemon 仍应答 {s}")
    finally:
        if host:
            host.stop()
    old = None
    try:
        data = os.path.join(WORK, "w4-old")
        old = SpawnedDaemon(OLD, data, capabilities=False)
        if not old.port:
            check("W4r", "反面对照：main 桌面方式起的 daemon 就绪", False, "没有 ready 行")
            return
        c1, s1 = cli(OLD, data, "daemon", "status")
        # An attach that fails falls back to spawning an ephemeral daemon; a
        # daemon binary that does not exist shuts that door, so 0 here can
        # only come from using the desktop's daemon.
        c2, _ = cli(OLD, data, "models", extra_env={"AGENT24D_BIN": os.path.join(WORK, "no-such-agent24d")})
        c3, _ = cli(OLD, data, "daemon", "stop")
        try:
            old.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            pass
        check("W4r", "反面对照：main 上 CLI 能用、也能停桌面起的 daemon",
              c1 == 0 and f"pid {old.proc.pid}" in s1 and c2 == 0 and c3 == 0 and not old.alive(),
              f"status {c1}，models {c2}，stop {c3}，之后 daemon {'还在' if old.alive() else '已退出'}")
    finally:
        if old:
            old.stop()


def case_w6():
    """Documenting's desktop path (the CLI with `--host-stdin`, documenting
    r6) against the daemon this release's desktop starts, and main's."""
    host = None
    try:
        data = os.path.join(WORK, "w6")
        code, said = install_documents(NEW, data)
        if code != 0:
            check("W6", "装上 documents 包", False, f"os install 退出 {code}：{said[-80:]}")
            return
        host = SpawnedDaemon(NEW, data)
        if not host.port:
            check("W6", "宿主 daemon 就绪", False, "没有 ready 行")
            return
        code, answer, waited = documents_ready(NEW, data, host)
        ok, status, err = result_of(answer)
        check("W6a", "桌面的方式（CLI `--host-stdin` 带宿主凭据）问 documents 模块，能答",
              code == 0 and ok is True and status == 200,
              f"退出 {code}，{status} {err or ''}，等了 {waited:.1f} 秒")
        code, answer = documents_request(NEW, data, host, {"op": "list"})
        ok, status, err = result_of(answer)
        payload = answer.get("data") if ok is True else None
        docs = payload.get("documents") if isinstance(payload, dict) else None
        check("W6b", "列文档：新数据目录里一个也没有", code == 0 and ok is True and status == 200 and docs == [],
              f"退出 {code}，{status} {err or ''}，documents={docs!r}")
        code, answer = documents_request(NEW, data, host, {"op": "list"}, token="wrong-" + secrets.token_hex(8))
        ok, status, err = result_of(answer)
        check("W6c", "凭据不对被拒，说明 W6a、W6b 的 200 确实经过了鉴权",
              code == 1 and ok is False and status == 401 and err == "unauthorized", f"退出 {code}，{status} {err}")
    finally:
        if host:
            host.stop()
    old = None
    try:
        data = os.path.join(WORK, "w6-old")
        code, said = install_documents(OLD, data)
        if code != 0:
            check("W6r", "对照：main 装上 documents 包", False, f"os install 退出 {code}：{said[-80:]}")
            return
        old = SpawnedDaemon(OLD, data, capabilities=False)
        if not old.port:
            check("W6r", "对照：main 桌面方式起的 daemon 就绪", False, "没有 ready 行")
            return
        code, answer, waited = documents_ready(OLD, data, old)
        ok, status, err = result_of(answer)
        check("W6r", "对照：main 的桌面方式（legacy daemon）同样能答",
              code == 0 and ok is True and status == 200, f"退出 {code}，{status} {err or ''}，等了 {waited:.1f} 秒")
    finally:
        if old:
            old.stop()


try:
    for case in (case_w1, case_w2, case_w3_w5, case_w4, case_w6):
        try:
            case()
        except Exception as err:  # a case that blows up is a FAIL, the rest still run
            check(case.__name__, "用例异常中止", False, repr(err))
        reap()
finally:
    reap()

changed = sorted(set(snapshot(REAL).items()) ^ set(Z1_BEFORE.items()))
check("Z1", "~/.agent24 没有被碰", not changed,
      "没有变化" if not changed else f"{len(changed)} 处变化，例如 {changed[0][0]}")

with open(os.path.join(WORK, "report.md"), "w") as f:
    f.write("| ID | 检查 | 结果 | 依据 |\n|---|---|---|---|\n" + "\n".join(RESULTS) + "\n")
print(f"\n{len(RESULTS) - FAILS} PASS, {FAILS} FAIL — {os.path.join(WORK, 'report.md')}")
sys.exit(1 if FAILS else 0)
