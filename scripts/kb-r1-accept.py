#!/usr/bin/env python3
"""Knowledge (ab/weknora) first release: acceptance on a real machine.

    python3 -I scripts/kb-r1-accept.py --old <main bin dir> --new <release bin dir> [--work <dir>]

Cases: docs/agent/KB-R1-ACCEPTANCE.md. Each bin dir holds `agent24d` and
`agent24` (`cargo build -p agent24d -p agent24-cli`). Every daemon runs on its
own AGENT24_DATA_DIR under --work; ~/.agent24 must stay untouched (Z1). No
model is needed. Prints PASS/FAIL per check, writes report.md in the work
directory and exits non-zero on any FAIL.
"""

import argparse
import http.client
import json
import os
import secrets
import select
import subprocess
import sys
import time

READY_TIMEOUT = 30
BINDING = "kb-accept"


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
if os.path.exists(WORK) and os.listdir(WORK):
    sys.exit(f"--work must be new or empty: {WORK}")
os.makedirs(WORK, exist_ok=True)

# Only what each case sets reaches a daemon or the CLI.
BASE_ENV = {k: v for k, v in os.environ.items() if not k.startswith(("A24_", "AGENT24_"))}

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
    """Every path under `top` with its size and mtime; empty if absent."""
    out = {}
    for root, dirs, files in os.walk(top):
        for name in dirs + files:
            path = os.path.join(root, name)
            try:
                st = os.lstat(path)
            except FileNotFoundError:
                continue
            out[path] = (st.st_size, st.st_mtime_ns)
    return out


def env_for(data):
    env = dict(BASE_ENV)
    env["AGENT24_DATA_DIR"] = data
    env["A24_OS_PACKAGES"] = data + ".pk"
    return env


def call(port, method, path, token=None, body=None):
    """(status, json or None, raw text). Host is 127.0.0.1:port, as the daemon's
    Host guard expects. Never raises: no answer is status 0."""
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
    except OSError as err:
        return 0, None, f"no answer: {err}"
    try:
        parsed = json.loads(raw) if raw else None
    except ValueError:
        parsed = None
    return res.status, parsed, raw


def code_of(parsed):
    if isinstance(parsed, dict) and isinstance(parsed.get("error"), dict):
        return parsed["error"].get("code")
    return None


class HostDaemon:
    """agent24d started the way the desktop starts it on macOS and Linux
    (apps/desktop/src/main/backend-manager.ts): capability mode, the ready
    record on a private stdout pipe, stdin held open as the parent lease."""

    def __init__(self, bin_dir, data):
        os.makedirs(data, exist_ok=True)
        self.data = data
        self.log_path = data + ".serve.log"
        self.log = open(self.log_path, "wb")
        self.proc = subprocess.Popen(
            [os.path.join(bin_dir, "agent24d"), "serve", "--port", "0",
             "--auth-mode", "capabilities", "--host-bootstrap-stdio"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log,
            env=env_for(data),
        )
        self.ready = None
        line = b""
        deadline = time.monotonic() + READY_TIMEOUT
        while not line.endswith(b"\n") and time.monotonic() < deadline:
            r, _, _ = select.select([self.proc.stdout], [], [], 0.5)
            if r:
                chunk = os.read(self.proc.stdout.fileno(), 65536)
                if not chunk:
                    break
                line += chunk
            elif self.proc.poll() is not None:
                break
        try:
            self.ready = json.loads(line) if line.strip() else None
        except ValueError:
            self.ready = None
        self.port = self.ready.get("port") if isinstance(self.ready, dict) else None
        self.token = self.ready.get("product_host_token") if isinstance(self.ready, dict) else None

    def stop(self):
        # stdin EOF ends the parent lease: the daemon shuts down by itself.
        try:
            self.proc.stdin.close()
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
        self.env = env_for(data)
        out = subprocess.run([os.path.join(bin_dir, "agent24"), "daemon", "start"],
                             env=self.env, capture_output=True, timeout=90)
        self.started = out.returncode == 0
        self.words = (out.stdout + out.stderr).decode("utf-8", "replace").strip()
        self.port = self.token = self.pid = None
        state = os.path.join(data, "daemon.json")
        if self.started and os.path.exists(state):
            with open(state) as f:
                s = json.load(f)
            self.port, self.token, self.pid = s.get("port"), s.get("token"), s.get("pid")

    def stop(self):
        subprocess.run([os.path.join(self.bin, "agent24"), "daemon", "stop"],
                       env=self.env, capture_output=True, timeout=60)
        if self.pid:
            for _ in range(40):
                try:
                    os.kill(self.pid, 0)
                except OSError:
                    return
                time.sleep(0.25)
            try:
                os.kill(self.pid, 9)
            except OSError:
                pass


def start_run(port, token, binding=None):
    body = {"prompt": "kb-r1 accept"}
    if binding is not None:
        body["knowledge_binding_id"] = binding
    return call(port, "POST", "/api/v1/runs", token, body)


def host_change(port, token, method, seq, suffix="", body=None):
    return call(port, method, f"/api/v1/host/knowledge/bindings/{BINDING}{suffix}?seq={seq}", token, body)


def files_containing(top, extra, needle):
    hits = []
    paths = list(extra)
    for root, _, files in os.walk(top):
        paths += [os.path.join(root, f) for f in files]
    for path in paths:
        try:
            with open(path, "rb") as f:
                if needle.encode() in f.read():
                    hits.append(os.path.relpath(path, WORK))
        except OSError:
            pass
    return hits


Z1_BEFORE = snapshot(REAL)


def case_w1():
    host = HostDaemon(NEW, os.path.join(WORK, "w1"))
    try:
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
        leaked = files_containing(host.data, [host.log_path], host.token)
        check("W1c", "宿主凭据只在 ready 管道里，不落盘、不进日志", not leaked,
              "数据目录和日志里都没有" if not leaked else f"出现在 {leaked}")
    finally:
        host.stop()


def case_w2():
    host = HostDaemon(NEW, os.path.join(WORK, "w2"))
    try:
        if not host.port:
            check("W2", "宿主 daemon 就绪", False, "没有 ready 行")
            return
        p, t = host.port, host.token
        s, run, _ = start_run(p, t)
        check("W2a", "不指名 binding 的 run 照常建立", s == 202 and isinstance(run, dict)
              and run.get("input", {}).get("knowledge_binding_id") is None,
              f"{s}，input.knowledge_binding_id={run.get('input', {}).get('knowledge_binding_id') if isinstance(run, dict) else None!r}")
        s, body, _ = start_run(p, t, BINDING)
        check("W2b", "指名没激活的 binding 被拒", s == 409 and code_of(body) == "knowledge_binding_inactive",
              f"{s} {code_of(body)}")
        s1, _, _ = host_change(p, t, "PUT", 1)
        s2, run, _ = start_run(p, t, BINDING)
        rid = run.get("id") if isinstance(run, dict) else None
        s3, shown, _ = call(p, "GET", f"/api/v1/runs/{rid}", t) if rid else (0, None, "")
        named = shown.get("input", {}).get("knowledge_binding_id") if isinstance(shown, dict) else None
        check("W2c", "激活后 run 能指名它，run 记着这个 binding", s1 == 204 and s2 == 202 and s3 == 200 and named == BINDING,
              f"激活 {s1}，建 run {s2}，读回 {s3} knowledge_binding_id={named!r}")
        s, body, _ = host_change(p, t, "PUT", 1)
        check("W2d", "过期的序号不能覆盖新的变更", s == 409 and code_of(body) == "stale_sequence",
              f"{s} {code_of(body)}")
        secret = "kbr1-" + secrets.token_hex(16)
        s1, b1, raw1 = host_change(p, t, "PUT", 2, "/credential",
                                   {"kind": "bearer", "value": secret, "expires_at": int(time.time()) + 600})
        s2, b2, raw2 = start_run(p, t, BINDING)
        leaked = files_containing(host.data, [host.log_path], secret)
        in_answers = secret in raw1 or secret in raw2
        check("W2e", "推送凭据后，应答、数据目录和日志里都没有它", s1 == 204 and s2 == 202 and not leaked and not in_answers,
              f"推送 {s1}，再建 run {s2}，落盘 {leaked or '无'}，应答里 {'有' if in_answers else '无'}")
        s1, _, _ = host_change(p, t, "DELETE", 3)
        s2, body, _ = start_run(p, t, BINDING)
        check("W2f", "停用后再指名被拒", s1 == 204 and s2 == 409 and code_of(body) == "knowledge_binding_inactive",
              f"停用 {s1}，建 run {s2} {code_of(body)}")
    finally:
        host.stop()
    # Reverse control: main's daemon has no host interface and no check.
    old = HostDaemon(OLD, os.path.join(WORK, "w2-old"))
    try:
        if not old.port:
            check("W2r", "反面对照：main 的宿主 daemon 就绪", False, "没有 ready 行")
            return
        s1, _, _ = host_change(old.port, old.token, "PUT", 1)
        s2, _, _ = start_run(old.port, old.token, BINDING)
        check("W2r", "反面对照：main 没有宿主接口，也不查 binding", s1 != 204 and s2 == 202,
              f"main 激活 {s1}，指名没激活的 binding 建 run {s2}")
    finally:
        old.stop()


def case_w3():
    cli = CliDaemon(NEW, os.path.join(WORK, "w3"))
    try:
        if not cli.port:
            check("W3", "`agent24 daemon start` 起来", False, cli.words)
            return
        p, t = cli.port, cli.token
        s, body, _ = start_run(p, t, BINDING)
        check("W3a", "普通 CLI 用户指名 binding 被拒", s == 403 and code_of(body) == "host_authority_required",
              f"{s} {code_of(body)}")
        s, body, _ = host_change(p, t, "PUT", 1)
        check("W3b", "普通 CLI 用户调宿主接口被拒", s == 403 and code_of(body) == "host_authority_required",
              f"{s} {code_of(body)}")
        s, _, _ = start_run(p, t)
        check("W3c", "不指名 binding 的 run 照常建立", s == 202, f"{s}")
    finally:
        cli.stop()
    old = CliDaemon(OLD, os.path.join(WORK, "w3-old"))
    try:
        if not old.port:
            check("W3r", "反面对照：main 的 `agent24 daemon start`", False, old.words)
            return
        s, _, _ = start_run(old.port, old.token, BINDING)
        check("W3r", "反面对照：main 不查 binding，照常建 run", s == 202, f"{s}")
    finally:
        old.stop()


for case in (case_w1, case_w2, case_w3):
    try:
        case()
    except Exception as err:  # a case that blows up is a FAIL, the rest still run
        check(case.__name__, "用例异常中止", False, repr(err))

changed = sorted(set(snapshot(REAL).items()) ^ set(Z1_BEFORE.items()))
check("Z1", "~/.agent24 没有被碰", not changed,
      "没有变化" if not changed else f"{len(changed)} 处变化，例如 {changed[0][0]}")

with open(os.path.join(WORK, "report.md"), "w") as f:
    f.write("| ID | 检查 | 结果 | 依据 |\n|---|---|---|---|\n" + "\n".join(RESULTS) + "\n")
print(f"\n{len(RESULTS) - FAILS} PASS, {FAILS} FAIL — {os.path.join(WORK, 'report.md')}")
sys.exit(1 if FAILS else 0)
