#!/usr/bin/env python3
"""KB-SEC-01b (#1089): once Knowledge evidence is in a run, no exit of the
daemon sends anything to a remote tier, end to end on a main build.

    python3 -I scripts/kb-sec-01b-accept.py --bin <dir with agent24d> [--work <dir>]

Cases: docs/agent/KB-SEC-01B-ACCEPTANCE.md. Everything runs on loopback and
is fake except the daemon: a scripted model as the local tier (OMLX_URL), the
same model behind a v4-mapped loopback address as the remote tier
(OLLAMA_URL; the router labels it Remote) that counts every request it gets,
a fake WeKnora in integration mode, a stdio MCP server that logs its calls.
The daemon runs in capability mode under its own AGENT24_DATA_DIR and HOME,
as desktop main starts it. Prints PASS/FAIL per check, writes report.md in
the work directory and exits non-zero on any FAIL.
"""

import argparse
import http.client
import json
import os
import re
import select
import subprocess
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BINDING = "kb-sec"
KB = "kb-sec-01b"
FACT = "蘑菇镇的图书馆每周三晚上八点闭馆（kb-sec-01b 合成样本）。"
STUB_MODEL = "kb-sec-stub"
REFUSED = "denied by policy"
FETCH_URL = "http://kb-sec-01b.invalid/"
# The memory case: a prompt the rule-based retain parser keeps as a fact. It
# holds no "SEC:" so the fact it leaves cannot be recalled by other prompts.
REMEMBER = "remember that the vault word is ZEBRA1089"
RECALL = "SEC:fetch what is the vault word ZEBRA1089"
RUN_TIMEOUT = 120


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--bin", required=True)
    p.add_argument("--work")
    return p.parse_args()


ARGS = parse_args()
BIN = os.path.realpath(ARGS.bin)
WORK = os.path.realpath(ARGS.work or os.path.join(os.environ.get("TMPDIR", "/tmp"), f"kb-sec-01b.{os.getpid()}"))
os.makedirs(WORK, exist_ok=True)
RESULTS = []
LOG = {"local": [], "remote": []}  # every request each model tier got: (method, path, body or None)
WK_CALLS = []  # (op, body) of every call the fake WeKnora got
MCP_LOG = os.path.join(WORK, "mcp-calls.log")


def check(cid, what, ok, evidence):
    RESULTS.append((cid, what, ok, evidence))
    print(f"{'PASS' if ok else 'FAIL'} {cid} {what} — {evidence}", flush=True)


def request(url, method, body=None, headers=None, timeout=30):
    """(status, json or None, raw). Never raises: no answer is status 0."""
    u = urllib.parse.urlsplit(url)
    headers = dict(headers or {})
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    try:
        conn = http.client.HTTPConnection(u.hostname, u.port, timeout=timeout)
        conn.request(method, u.path + (f"?{u.query}" if u.query else ""), body=data, headers=headers)
        res = conn.getresponse()
        raw = res.read().decode("utf-8", "replace")
        conn.close()
    except (OSError, http.client.HTTPException) as err:
        return 0, None, f"no answer: {err!r}"
    try:
        return res.status, json.loads(raw) if raw else None, raw
    except ValueError:
        return res.status, None, raw


class Quiet(BaseHTTPRequestHandler):
    def answer(self, body, code=200):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def log_message(self, *args):
        pass


def serve(handler, host="127.0.0.1"):
    server = ThreadingHTTPServer((host, 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


# ---- the scripted model: one plan per prompt ----

def text_of(m):
    c = m.get("content")
    return c if isinstance(c, str) else json.dumps(c, ensure_ascii=False)


def plan_of(text):
    # Recalled memory comes in the same message, ahead of the prompt.
    text = text.rsplit("[记忆数据结束]", 1)[-1].strip()
    if text.startswith(REMEMBER):
        return ["ks"]
    found = re.search(r"SEC:([a-z,]*)", text)
    return [s for s in found.group(1).split(",") if s] if found else None


def step_call(step, text, offered):
    """The tool call for one plan step: (name, arguments)."""
    arg = lambda key: (re.search(rf"{key}=(\S+)", text) or [None, ""])[1]
    if step == "ks":
        return "knowledge_search", {"query": "蘑菇镇 图书馆"}
    if step == "fetch":
        return "http_fetch", {"url": FETCH_URL}
    if step == "exec":
        return "shell_exec", {"argv": ["touch", arg("FILE")]}
    if step == "echo":
        return "shell_exec", {"argv": ["echo", "SHELL-OUT-1089"]}
    if step == "write":
        return "fs_write", {"path": arg("FILE"), "content": "kb-sec-01b"}
    if step == "explore":
        return "explore", {"task": "SEC: SUB-1089 回答一句话即可"}
    if step == "wake":
        return "self_wake", {"prompt": f"SEC:fetch WAKE-{arg('TAG')}", "after_secs": 1}
    if step == "mcp":
        return next((n for n in offered if n.startswith("mcp") and n.endswith("ping")), "mcp-probe-ping"), {}
    raise ValueError(step)


def stub_reply(req):
    msgs = req.get("messages") or []
    say = lambda text: {"role": "assistant", "content": text}
    if any(text_of(m).startswith(("Summarize this conversation", "Update this running summary")) for m in msgs):
        return say("SUMMARY-1089")
    at = next((i for i in range(len(msgs) - 1, -1, -1) if msgs[i].get("role") == "user"
               and plan_of(text_of(msgs[i])) is not None), None)
    if at is None:
        return say("STUB-DONE")
    text, turn = text_of(msgs[at]), msgs[at:]
    plan = plan_of(text)
    done = sum(1 for m in turn if m.get("role") == "assistant" and m.get("tool_calls"))
    results = [text_of(m) for m in turn if m.get("role") == "tool"]
    if done >= len(plan):
        return say("DONE " + json.dumps(results, ensure_ascii=False))
    offered = [t.get("function", {}).get("name") for t in req.get("tools") or []]
    name, args = step_call(plan[done], text, offered)
    if name not in offered:
        return say(f"NOT-OFFERED {name}")
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call_sec_{len(LOG['local']) + len(LOG['remote'])}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(args, ensure_ascii=False)}}]}


def model(tier):
    class Model(Quiet):
        def do_GET(self):
            LOG[tier].append(("GET", self.path, None))
            self.answer({"object": "list", "data": [{"id": STUB_MODEL, "object": "model"}]})

        def do_POST(self):
            req = json.loads(self.body() or b"{}")
            LOG[tier].append(("POST", self.path, req))
            message = stub_reply(req)
            finish = "tool_calls" if message.get("tool_calls") else "stop"
            self.answer({"id": "kb-sec", "object": "chat.completion", "model": STUB_MODEL,
                         "choices": [{"index": 0, "finish_reason": finish, "message": message}],
                         "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
    return Model


# ---- a fake WeKnora in integration mode (contract v1, protocol/knowledge/fixtures/v1) ----

class FakeWeKnora(Quiet):
    def do_GET(self):
        if not self.path.endswith("/system/capabilities"):
            return self.answer({"error": {"code": "not_found"}}, 404)
        self.answer({"edition": "lite", "integration": {"agent24_knowledge": {
            "protocol_versions": [1], "mode": "integration", "supported_operations": ["search", "read"],
            "server_version": "kb-sec-01b-fake", "limits": {
                "max_results": 50, "max_concurrency": 4, "max_deadline_ms": 30000,
                "max_read_bytes": 262144, "max_page_limit": 100, "max_snippet_bytes": 2048}}}})

    def do_POST(self):
        req = json.loads(self.body() or b"{}")
        op = self.path.rsplit("/", 1)[-1]
        WK_CALLS.append((op, req))
        echo = {"protocol_version": 1, "request_id": req.get("request_id"), "generation": req.get("generation")}
        if op == "ready":
            return self.answer({**echo, "live": True,
                                "operations": {"search": {"state": "ready"}, "read": {"state": "ready"}}})
        scope = req.get("scope") or {"knowledge_base_ids": [KB]}
        if op == "search":
            budget = req.get("budget") or {}
            return self.answer({**echo, "completeness": "complete", "effective_scope": scope,
                                "applied_budget": {"deadline_ms": budget.get("deadline_ms", 8000),
                                                   "max_results": min(budget.get("max_results", 50), 50),
                                                   "max_concurrency": min(budget.get("max_concurrency", 2), 4)},
                                "hits": [{"rank": 1, "knowledge_base_id": KB, "knowledge_id": "doc-sec", "chunk_id": "chk-sec",
                                          "chunk_type": "text", "revision": {"source_revision": "src-1", "index_build_id": "ib-1",
                                                                             "chunk_revision": 0, "content_tag": "ct-sec"},
                                          "locator": {"valid": False}, "index_state": "fresh", "score": 0.9,
                                          "score_space": "kw", "snippet": {"text": FACT, "truncated": False},
                                          "display": {"title": "蘑菇镇样本", "file_name": "mushroom-town.txt"}}]})
        if op == "read":
            return self.answer({**echo, "effective_scope": scope, "knowledge_id": "doc-sec",
                                "revision": {"source_revision": "src-1", "index_build_id": "ib-1"},
                                "segments": [{"chunk_id": "chk-sec", "chunk_type": "text", "chunk_revision": 0,
                                              "content_tag": "ct-sec", "text": FACT}], "truncated": False})
        self.answer({"error": {"code": "not_found"}}, 404)


MCP_SERVER = r'''
import json, sys
log = sys.argv[1]
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method, result = msg.get("method"), None
    if method == "initialize":
        result = {"protocolVersion": msg["params"].get("protocolVersion", "2024-11-05"),
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "probe", "version": "0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "ping", "description": "Answers PONG.",
                             "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        with open(log, "a") as f:
            f.write(json.dumps(msg["params"]) + "\n")
        result = {"content": [{"type": "text", "text": "PONG-1089"}]}
    reply = {"jsonrpc": "2.0", "id": msg["id"]}
    reply.update({"result": result} if result is not None else {"error": {"code": -32601, "message": "no such method"}})
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
'''


def mcp_calls():
    try:
        with open(MCP_LOG) as f:
            return len(f.readlines())
    except FileNotFoundError:
        return 0


# ---- the daemon, as desktop main runs it ----

def read_ready(proc, timeout):
    deadline = time.time() + timeout
    fd = proc.stdout.fileno()
    buf = b""
    while b"\n" not in buf:
        left = deadline - time.time()
        if left <= 0 or not select.select([fd], [], [], left)[0]:
            return {}
        chunk = os.read(fd, 4096)
        if not chunk:
            return {}
        buf += chunk
    try:
        line = json.loads(buf.split(b"\n", 1)[0])
    except ValueError:
        return {}
    return line if isinstance(line, dict) else {}


class Daemon:
    def __init__(self, data, model_env):
        self.data, self.model_env = data, model_env
        os.makedirs(os.path.join(data, "workspace"), exist_ok=True)
        os.makedirs(data + ".home", exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("A24_", "AGENT24", "OMLX_", "OLLAMA_", "IDORIS_"))}
        env.pop("DEFAULT_MODEL", None)
        env.update(model_env, AGENT24_DATA_DIR=data, A24_OS_PACKAGES=data + ".pk", HOME=data + ".home",
                   A24_SCHEDULER_TICK_SECS="1", DEFAULT_MODEL=STUB_MODEL)
        self.log = open(data + ".serve.log", "ab")
        self.proc = subprocess.Popen(
            [os.path.join(BIN, "agent24d"), "serve", "--port", "0", "--auth-mode", "capabilities",
             "--host-bootstrap-stdio"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, env=env, cwd=data)
        line = read_ready(self.proc, 30)
        self.port, self.token = line.get("port"), line.get("product_host_token")
        if line.get("type") != "ready" or not isinstance(self.port, int) or not isinstance(self.token, str):
            self.stop()
            sys.exit(f"daemon did not come up; see {data}.serve.log")
        self.seq = int(time.time() * 1000)

    def call(self, method, path, body=None):
        return request(f"http://127.0.0.1:{self.port}{path}", method, body, {"Authorization": f"Bearer {self.token}"})

    def bind(self, endpoint, binding=BINDING, key="kb-sec-01b-retrieve-key"):
        """(status of activating, status of pushing the credential) the way desktop main does it."""
        # K-B6 (#1714): the sequence is in the JSON body, never in the query.
        self.seq += 1
        s1, _, _ = self.call("PUT", f"/api/v1/host/knowledge/bindings/{binding}",
                             {"seq": self.seq, "endpoint": endpoint, "knowledge_base_ids": [KB]})
        self.seq += 1
        s2, _, _ = self.call("PUT", f"/api/v1/host/knowledge/bindings/{binding}/credential",
                             {"seq": self.seq, "kind": "api_key", "value": key, "expires_at": int(time.time()) + 3600})
        return s1, s2

    def session(self):
        status, got, raw = self.call("POST", "/api/v1/sessions", {"title": "", "channel": "desktop"})
        if status not in (200, 201) or not isinstance(got, dict):
            sys.exit(f"session not created: {status} {raw[:200]}")
        return got["id"]

    def start(self, prompt, binding=BINDING, session=None):
        body = {"prompt": prompt, "session_id": session or self.session()}
        if binding:
            body["knowledge_binding_id"] = binding
        status, run, raw = self.call("POST", "/api/v1/runs", body)
        if status not in (200, 201, 202) or not isinstance(run, dict):
            sys.exit(f"run not created: {status} {raw[:200]}")
        return run["id"]

    def pending(self, run_id):
        _, got, _ = self.call("GET", "/api/v1/approvals?status=pending")
        return [a for a in (got or {}).get("approvals") or [] if a.get("run_id") == run_id]

    def wait(self, run_id, approve=True, until=None):
        """The run once it ends (approving what it asks for), or once `until(self)` holds."""
        deadline = time.time() + RUN_TIMEOUT
        while time.time() < deadline:
            time.sleep(0.3)
            if until is not None and until(self):
                return {"status": "until"}
            if approve:
                for a in self.pending(run_id):
                    self.call("POST", f"/api/v1/approvals/{a['id']}", {"type": "approve"})
            _, got, _ = self.call("GET", f"/api/v1/runs/{run_id}")
            if isinstance(got, dict) and got.get("status") in ("completed", "failed", "cancelled"):
                return got
        self.call("POST", f"/api/v1/runs/{run_id}/cancel")
        return {"status": "timeout"}

    def ask(self, prompt, binding=BINDING, session=None):
        return self.wait(self.start(prompt, binding, session))

    def stop(self, kill=False):
        if kill:
            self.proc.kill()
        else:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc.wait()
        self.log.close()

    def restart(self, kill=False):
        self.stop(kill)
        self.__init__(self.data, self.model_env)


def results_of(run):
    """The tool answers the run's model got, as the scripted model echoed them."""
    text = ((run or {}).get("output") or {}).get("text") or ""
    try:
        return json.loads(text[5:]) if text.startswith("DONE ") else [f"<answer: {text[:200]}>"]
    except ValueError:
        return [f"<answer: {text[:200]}>"]


def got_evidence(run):
    """The run's first tool answer is a knowledge_search that brought the sample
    back: a failed search is tagged Unknown too, so a refusal after one would
    not show what Knowledge evidence does (#1089 review C1)."""
    r = results_of(run)
    return bool(r) and FACT in str(r[0]) and '"kind":"knowledge.search"' in str(r[0])


def refused(result):
    return str(result).startswith(REFUSED)


def remote_count():
    return len(LOG["remote"])


def show(run):
    brief = [("knowledge.search 证据" if '"kind":"knowledge.search"' in str(x) else str(x)[:90]) for x in results_of(run)]
    return f"状态 {(run or {}).get('status')}；工具结果 {json.dumps(brief, ensure_ascii=False)}"


def cases(d, wk):
    s1, s2 = d.bind(wk)
    check("E0", "绑定按 desktop main 的做法激活并推入凭据", s1 == 204 and s2 == 204, f"PUT binding {s1}；PUT credential {s2}；端点 {wk}")

    # Controls: the same exits from a run with no Knowledge evidence go out.
    # One exit per run: an MCP or shell result is Unknown and would restrict the next.
    ctl = os.path.join(d.data, "workspace", "control.touched")
    runs = [d.ask(p, binding=None) for p in ("SEC:fetch", "SEC:mcp", f"SEC:exec FILE={ctl}")]
    r = [x for run in runs for x in results_of(run)]
    check("O1", "对照：没有 Knowledge 证据的 run，http_fetch（到 DNS 这一步）、MCP、shell_exec 都照常放行",
          len(r) == 3 and "dns lookup failed" in r[0] and r[1] == "PONG-1089" and os.path.exists(ctl) and mcp_calls() == 1,
          f"{'；'.join(show(run) for run in runs)}；control 文件 {'在' if os.path.exists(ctl) else '不在'}；MCP 收到 {mcp_calls()} 次")

    run = d.ask("SEC:ks")
    r = results_of(run)
    local_saw = any(FACT in json.dumps(b, ensure_ascii=False) for _, _, b in LOG["local"] if b)
    check("A1", "回答：带绑定的 run 检索到证据并作答，证据只到本地档",
          run.get("status") == "completed" and len(r) == 1 and FACT in r[0] and local_saw, show(run))

    run = d.ask("SEC:ks,fetch")
    r = results_of(run)
    check("H1", "HTTP：有 Knowledge 证据后 http_fetch 在发出前被拒",
          got_evidence(run) and len(r) == 2 and refused(r[1]), show(run))

    gone = os.path.join(d.data, "workspace", "exec.touched")
    run = d.ask(f"SEC:ks,exec FILE={gone}")
    r = results_of(run)
    check("X1", "Exec：有 Knowledge 证据后 shell_exec 被拒，子进程没跑",
          got_evidence(run) and len(r) == 2 and refused(r[1]) and not os.path.exists(gone), f"{show(run)}；文件 {'在' if os.path.exists(gone) else '不在'}")

    before = mcp_calls()
    run = d.ask("SEC:ks,mcp")
    r = results_of(run)
    check("P1", "MCP：有 Knowledge 证据后 MCP 工具被拒，MCP server 没收到调用",
          got_evidence(run) and len(r) == 2 and refused(r[1]) and mcp_calls() == before, f"{show(run)}；MCP 调用 {before}→{mcp_calls()}")

    n = len(LOG["local"])
    run = d.ask("SEC:ks,explore")
    r = results_of(run)
    sub = [b for _, _, b in LOG["local"][n:] if b and any("SUB-1089" in text_of(m) for m in b.get("messages") or [])]
    check("D1", "委派：有 Knowledge 证据后 explore 子调用只到本地档",
          run.get("status") == "completed" and got_evidence(run) and len(r) == 2 and bool(sub), f"{show(run)}；本地档收到子调用 {len(sub)} 次")

    s = d.session()
    first = d.ask("SEC:ks", session=s)
    run = d.ask("SEC:fetch", session=s)
    r = results_of(run)
    check("C1", "续轮：同一会话上一轮有 Knowledge 证据，下一轮 http_fetch 被拒",
          first.get("status") == "completed" and got_evidence(first) and len(r) == 1 and refused(r[0]), show(run))

    tag = str(int(time.time()))
    run = d.ask(f"SEC:ks,wake TAG={tag}")
    woken = {}
    deadline = time.time() + 60
    while time.time() < deadline and not woken:
        time.sleep(1)
        _, got, _ = d.call("GET", "/api/v1/runs")
        woken = next((x for x in (got or {}).get("runs") or [] if f"WAKE-{tag}" in json.dumps(x.get("input"), ensure_ascii=False)), {})
    woken = d.wait(woken["id"]) if woken.get("id") else {}
    r = results_of(woken)
    check("B1", "后台：有 Knowledge 证据的 run 建的 self_wake，被唤醒的 run 里 http_fetch 被拒",
          got_evidence(run) and woken.get("status") == "completed" and len(r) == 1 and refused(r[0]),
          f"建唤醒的 {show(run)}；被唤醒的 {show(woken)}")

    park = os.path.join(d.data, "workspace", "resume.txt")
    run_id = d.start(f"SEC:ks,write,fetch FILE={park}")
    parked = d.wait(run_id, approve=False, until=lambda dd: bool(dd.pending(run_id)))
    d.restart(kill=True)
    d.bind(wk)
    run = d.wait(run_id)
    r = results_of(run)
    check("R1", "恢复：停在审批上时 daemon 被 kill -9，重启批准后，http_fetch 仍被拒",
          parked.get("status") == "until" and got_evidence(run) and len(r) == 3
          and str(r[1]).startswith("wrote ") and os.path.exists(park) and refused(r[2]),
          f"停下 {parked.get('status')}；恢复后 {show(run)}")

    s = d.session()
    first = d.ask("SEC:ks", session=s)
    for i in range(20):
        d.ask(f"SEC: turn {i}", binding=None, session=s)
    deadline = time.time() + 30
    summaries = lambda tier: [b for _, _, b in LOG[tier] if b and any(text_of(m).startswith(("Summarize this conversation", "Update this running summary"))
                                      for m in b.get("messages") or [])]
    while time.time() < deadline and not summaries("local"):
        time.sleep(1)
    check("S1", "摘要：带 Knowledge 证据的会话做压缩，摘要调用只到本地档",
          got_evidence(first) and bool(summaries("local")) and not summaries("remote"), f"本地档摘要调用 {len(summaries('local'))} 次，远端档 {len(summaries('remote'))} 次")

    # Agent24 -> WeKnora (K1-DEPENDENCIES G1(a), K1-D9).
    searches = sum(1 for op, _ in WK_CALLS if op == "search")
    run = d.ask("SEC:echo,ks")
    r = results_of(run)
    after = sum(1 for op, _ in WK_CALLS if op == "search")
    check("T1", "传输：run 里有非 Knowledge 的 Unknown 来源（shell 输出）时，knowledge_search 在发往 WeKnora 前被拒",
          len(r) == 2 and not refused(r[0]) and refused(r[1]) and after == searches,
          f"{show(run)}；WeKnora search {searches}→{after}")
    far, _ = d.bind("http://192.0.2.10/api/v1", binding="kb-sec-far")
    check("T2", "传输：明文 http 的非回环端点激活不了", far == 400, f"PUT binding {far}")

    # Memory last: the Unknown fact it leaves is recalled by its words only.
    first = d.ask(REMEMBER, session=d.session())
    n = len(LOG["local"])
    run = d.ask(RECALL, binding=None)
    r = results_of(run)
    recalled = any("ZEBRA1089" in text_of(m) and text_of(m).startswith("[记忆数据")
                   for _, _, b in LOG["local"][n:] if b for m in b.get("messages") or [])
    check("M1", "记忆：有 Knowledge 证据的 run 记下的事实，在别的会话被召回后，http_fetch 被拒",
          got_evidence(first) and recalled and len(r) == 1 and refused(r[0]),
          f"记下 {first.get('status')}；召回 {'有' if recalled else '没有'}；{show(run)}")


def local_only(remote_url):
    """A daemon with only the remote tier: every model path fails as local-only, none reaches it."""
    # OMLX_URL too: left unset it defaults to a loopback (local) address.
    d = Daemon(os.path.join(WORK, "remote-only"), {"OMLX_URL": remote_url, "OLLAMA_URL": remote_url})
    try:
        run = d.ask("SEC:ks", binding=None)
        err = (run.get("error") or {}).get("message") or ""
        status, _, raw = d.call("POST", "/api/v1/chat", {"messages": [{"role": "user", "content": "SEC: hello"}]})
    finally:
        d.stop()
    check("L1", "LocalOnly：只有远端档时，run 和 /api/v1/chat 都以 local-only 失败",
          run.get("status") == "failed" and "local-only" in err and status == 503 and "local-only" in raw,
          f"run {run.get('status')} {err[:120]!r}；chat {status} {raw[:120]!r}")


def main():
    servers, d = [], None
    try:
        local, remote, wk = serve(model("local")), serve(model("remote")), serve(FakeWeKnora)
        servers = [local, remote, wk]
        data = os.path.join(WORK, "data")
        os.makedirs(data, exist_ok=True)
        server_py = os.path.join(WORK, "mcp-probe.py")
        with open(server_py, "w") as f:
            f.write(MCP_SERVER)
        with open(os.path.join(data, "mcp.json"), "w") as f:
            json.dump({"mcpServers": {"probe": {"command": sys.executable, "args": ["-I", server_py, MCP_LOG]}}}, f)
        remote_url = f"http://[::ffff:127.0.0.1]:{remote.server_address[1]}"
        d = Daemon(data, {"OMLX_URL": f"http://127.0.0.1:{local.server_address[1]}", "OLLAMA_URL": remote_url})
        cases(d, f"http://127.0.0.1:{wk.server_address[1]}/api/v1")
        d.stop()
        d = None
        local_only(remote_url)
        check("Z1", "全程远端档收到的请求数为 0", remote_count() == 0,
              f"远端档 {remote_count()} 次（{[p for _, p, _ in LOG['remote']][:5]}）；本地档 {len(LOG['local'])} 次")
    finally:
        try:
            if d is not None:
                d.stop()
        finally:
            for server in servers:
                server.shutdown()
                server.server_close()
    with open(os.path.join(WORK, "report.md"), "w") as f:
        f.write(f"# KB-SEC-01b 全路径出口验收\n\n- daemon：{BIN}/agent24d\n- 时间（UTC）：{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n\n")
        f.write("| 编号 | 检查 | 结果 | 证据 |\n|---|---|---|---|\n")
        for cid, what, ok, ev in RESULTS:
            f.write(f"| {cid} | {what} | {'PASS' if ok else 'FAIL'} | {ev.replace('|', '\\|').replace(chr(10), ' ')} |\n")
    print(f"report: {os.path.join(WORK, 'report.md')}")
    return 0 if RESULTS and all(ok for _, _, ok, _ in RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
