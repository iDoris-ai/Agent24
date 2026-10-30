#!/usr/bin/env python3
"""Knowledge M5 (#1001 exit 1): one main-chat turn searches a sample library,
answers from it and cites it, end to end against a real WeKnora.

    python3 -I scripts/kb-m5-accept.py --bin <dir with agent24d> \
        --weknora http://127.0.0.1:<app port> [--work <dir>] \
        [--model-url <Ollama URL> --model <name>]

Cases: docs/agent/KB-M5-ACCEPTANCE.md. The daemon runs in capability mode
under its own AGENT24_DATA_DIR, as the desktop starts it, and the script does
what desktop main does (apps/desktop/src/main/knowledge-settings.ts): activate
the binding with the endpoint, push the retrieve key as C, create a session,
then a run naming the binding. WeKnora gets a fresh user, a keyword-only
knowledge base with one sample document and a retrieve key for that base;
its embedding calls go to a fake Ollama on 127.0.0.1:11434 (v0.8.x needs one
even for keyword search). By default the model is a scripted stub on
loopback: search, read the hit back as a citation, answer from it with a
source list. With --model-url the run uses that Ollama model instead, through
a recorder, and E0–E3 check what it did. Prints PASS/FAIL per check, writes report.md in the
work directory and exits non-zero on any FAIL.
"""

import argparse
import hashlib
import http.client
import json
import os
import secrets
import select
import subprocess
import sys
import threading
import time
import urllib.parse
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BINDING = "kb-m5"
MARKER = "M5-ASK"
FAKE_EMBED = "kb-m5-fake-embed:latest"
FACT = "蘑菇镇的图书馆每周三晚上八点闭馆，馆长名叫林小满。"
# What evidence, and an answer, must carry of FACT.
FACT_PARTS = ("每周三晚上八点闭馆", "林小满")
QUESTION = "蘑菇镇的图书馆每周三几点闭馆？馆长叫什么？"
RUN_TIMEOUT = 600
STUB_MODEL = "kb-m5-stub"


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--bin", required=True)
    p.add_argument("--weknora", required=True)
    p.add_argument("--work")
    p.add_argument("--model-url")
    p.add_argument("--model")
    a = p.parse_args()
    if bool(a.model_url) != bool(a.model):
        p.error("--model-url and --model go together")
    return a


ARGS = parse_args()
BIN = os.path.realpath(ARGS.bin)
WK = ARGS.weknora.rstrip("/")
WORK = os.path.realpath(ARGS.work or os.path.join(os.environ.get("TMPDIR", "/tmp"), f"kb-m5-accept.{os.getpid()}"))
REAL = os.path.realpath(os.path.join(os.path.expanduser("~"), ".agent24"))
if WORK == REAL or WORK.startswith(REAL + os.sep):
    sys.exit(f"--work must not be inside {REAL}")
if os.path.exists(WORK) and (not os.path.isdir(WORK) or os.listdir(WORK)):
    sys.exit("--work must not exist yet, or be an empty directory")
os.makedirs(WORK, exist_ok=True)
RESULTS = []


def check(cid, what, ok, evidence):
    RESULTS.append((cid, what, ok, evidence))
    print(f"{'PASS' if ok else 'FAIL'} {cid} {what} — {evidence}", flush=True)


def request(url, method, body=None, headers=None, form=None, timeout=30):
    """(status, json or None, raw). Never raises: no answer is status 0."""
    u = urllib.parse.urlsplit(url)
    headers = dict(headers or {})
    data = None
    if form is not None:
        boundary = uuid.uuid4().hex
        name, content = form
        data = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
                f"Content-Type: text/plain\r\n\r\n").encode() + content + f"\r\n--{boundary}--\r\n".encode()
        headers["Content-Type"] = f"multipart/form-data; boundary={boundary}"
    elif body is not None:
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


def need(status, parsed, raw, what, ok=(200, 201)):
    if status not in ok:
        sys.exit(f"setup failed: {what}: {status} {raw[:300]}")
    return parsed


# ---- loopback model side: fake embeddings, the scripted stub, the recorder ----

class Quiet(BaseHTTPRequestHandler):
    def answer(self, body, code=200):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def log_message(self, *args):
        pass


class FakeEmbed(Quiet):
    def do_GET(self):
        self.answer({"models": [{"name": FAKE_EMBED, "model": FAKE_EMBED, "size": 1, "digest": "0" * 64}]})

    def do_POST(self):
        r = json.loads(self.body() or b"{}")
        texts = r.get("input") or ""
        texts = texts if isinstance(texts, list) else [texts]
        vec = lambda t: [b / 255 for b in hashlib.sha256(t.encode()).digest()[:8]]
        self.answer({"model": FAKE_EMBED, "embeddings": [vec(str(t)) for t in texts]})


DUMPS = []


def stub_reply(req):
    """One step of the scripted model. `M5-ASK`: search, read the first hit
    back, then answer from the citation it got, with a source list.
    `M5-READ <knowledge_id> <chunk_id> <content_tag>`: read that
    citation back, then answer with its text. `M5-TWICE`: search, search
    again, then say what the second call got. `M5-FETCH`: search, then
    http_fetch a public page, then say what the fetch got."""
    turn = turn_of(req)
    offered = [t.get("function", {}).get("name") for t in req.get("tools") or []]
    say = lambda text: {"role": "assistant", "content": text}
    call = lambda name, args: {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call_{len(DUMPS)}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(args, ensure_ascii=False)}}]}
    words = str(turn[0].get("content")).split() if turn else []
    results = [str(m.get("content")) for m in turn if m.get("role") == "tool"]
    search = {"query": "蘑菇镇 图书馆 闭馆 馆长"}
    if not words or words[0] not in ("M5-ASK", "M5-READ", "M5-TWICE", "M5-FETCH"):
        return say("STUB-DONE")
    if "knowledge_search" not in offered:
        return say("NOT-OFFERED")
    if words[0] == "M5-READ":
        if not results:
            return call("knowledge_read", dict(zip(("knowledge_id", "chunk_id", "content_tag"), words[1:4])))
        return say(results[0])
    if not results:
        return call("knowledge_search", search)
    if words[0] == "M5-TWICE":
        return call("knowledge_search", search) if len(results) == 1 else say(f"SECOND {results[1]}")
    if words[0] == "M5-FETCH":
        if "http_fetch" not in offered:
            return say("FETCH-NOT-OFFERED")
        return call("http_fetch", {"url": "https://example.com/"}) if len(results) == 1 else say(f"FETCH {results[1][:300]}")
    hits = evidence_of(results[0])
    if not hits:
        return say(f"NO-EVIDENCE {results[0][:300]}")
    hit = hits[0]
    if len(results) == 1:
        return call("knowledge_read", {k: hit[k] for k in ("knowledge_id", "chunk_id", "content_tag")})
    cited = citations_of(results[1:])
    if not cited:
        return say(f"NO-CITATION {results[1][:300]}")
    title = (hit.get("display") or {}).get("title") or hit["knowledge_id"]
    key, text = next(iter(cited.items()))
    return say(f"{text.strip()}\n\n来源 / Sources\n- {title} {key}")


class Stub(Quiet):
    def do_GET(self):
        self.answer({"object": "list", "data": [{"id": STUB_MODEL, "object": "model"}]})

    def do_POST(self):
        req = json.loads(self.body() or b"{}")
        DUMPS.append(req)
        message = stub_reply(req)
        finish = "tool_calls" if message.get("tool_calls") else "stop"
        self.answer({"id": "kb-m5", "object": "chat.completion", "model": STUB_MODEL,
                     "choices": [{"index": 0, "finish_reason": finish, "message": message}],
                     "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})


class Recorder(Quiet):
    """Passes every call to --model-url unchanged and keeps each request body."""

    def forward(self, method):
        raw = self.body() if method == "POST" else None
        if raw:
            try:
                DUMPS.append(json.loads(raw))
            except ValueError:
                pass
        u = urllib.parse.urlsplit(ARGS.model_url)
        conn = http.client.HTTPConnection(u.hostname, u.port, timeout=RUN_TIMEOUT)
        conn.request(method, self.path, body=raw, headers={"Content-Type": "application/json"})
        res = conn.getresponse()
        data = res.read()
        self.send_response(res.status)
        self.send_header("Content-Type", res.getheader("Content-Type") or "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.forward("GET")

    def do_POST(self):
        self.forward("POST")


def serve(handler, port=0):
    server = ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


# ---- WeKnora: a user, a sample library and a retrieve key ----

def weknora_setup():
    rid = secrets.token_hex(4)
    pw = "m5-" + secrets.token_hex(12)
    email = f"kb-m5-{rid}@probe.invalid"
    need(*request(f"{WK}/api/v1/auth/register", "POST",
                  {"username": f"kb-m5-{rid}", "email": email, "password": pw}), "register")
    login = need(*request(f"{WK}/api/v1/auth/login", "POST", {"email": email, "password": pw}), "login")
    jwt = {"Authorization": f"Bearer {login['token']}"}
    tenant = login["user"]["tenant_id"]
    model = need(*request(f"{WK}/api/v1/models", "POST", {
        "name": FAKE_EMBED, "type": "Embedding", "source": "local",
        "parameters": {"embedding_parameters": {"dimension": 8}}}, jwt), "embedding model")["data"]["id"]
    kb = need(*request(f"{WK}/api/v1/knowledge-bases", "POST", {
        "name": f"kb-m5-{rid}", "embedding_model_id": model,
        "indexing_strategy": {"vector_enabled": False, "keyword_enabled": True}}, jwt), "knowledge base")["data"]["id"]
    sample = f"蘑菇镇样本资料（kb-m5 {rid}，纯属合成）。\n{FACT}\n镇上的面包店每天早上六点开门。\n".encode()
    doc = need(*request(f"{WK}/api/v1/knowledge-bases/{kb}/knowledge/file", "POST", None, jwt,
                        form=("mushroom-town.txt", sample)), "upload")["data"]["id"]
    status = None
    for _ in range(48):
        got = need(*request(f"{WK}/api/v1/knowledge/{doc}", "GET", None, jwt), "parse status")
        status = got["data"].get("parse_status")
        if status in ("completed", "failed"):
            break
        time.sleep(5)
    if status != "completed":
        sys.exit(f"setup failed: sample document parse_status={status}")
    key = need(*request(f"{WK}/api/v1/tenants/{tenant}/api-keys", "POST", {
        "name": f"kb-m5-{rid}", "capabilities": ["retrieve"], "knowledge_base_ids": [kb]}, jwt), "api key")["data"]["token"]
    return {"kb": kb, "doc": doc, "key": key}


# ---- the daemon, as desktop main runs it ----

def read_ready(proc, timeout):
    """The first stdout line as JSON, within `timeout` seconds in all; {} when
    there is none by then, or it is no JSON object."""
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
        os.makedirs(data, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("A24_", "AGENT24", "OMLX_", "OLLAMA_"))}
        env.pop("DEFAULT_MODEL", None)
        env.update(model_env, AGENT24_DATA_DIR=data, A24_OS_PACKAGES=data + ".pk", HOME=data + ".home")
        self.log = open(data + ".serve.log", "wb")
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
        return request(f"http://127.0.0.1:{self.port}{path}", method, body,
                       {"Authorization": f"Bearer {self.token}"})

    def host(self, method, suffix="", body=None):
        self.seq += 1
        return self.call(method, f"/api/v1/host/knowledge/bindings/{BINDING}{suffix}?seq={self.seq}", body)

    def session(self):
        status, session, raw = self.call("POST", "/api/v1/sessions", {"title": "", "channel": "desktop"})
        if status not in (200, 201) or not isinstance(session, dict):
            sys.exit(f"session not created: {status} {raw[:200]}")
        return session["id"]

    def ask(self, prompt, binding, session=None):
        body = {"prompt": prompt, "session_id": session or self.session()}
        if binding:
            body["knowledge_binding_id"] = binding
        status, run, raw = self.call("POST", "/api/v1/runs", body)
        if status not in (200, 201, 202):
            return status, None, raw
        deadline = time.time() + RUN_TIMEOUT
        while time.time() < deadline:
            time.sleep(1)
            _, got, _ = self.call("GET", f"/api/v1/runs/{run['id']}")
            if isinstance(got, dict) and got.get("status") in ("completed", "failed", "cancelled"):
                return status, got, ""
        self.call("POST", f"/api/v1/runs/{run['id']}/cancel")
        return status, {"status": "timeout"}, ""

    def stop(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.log.close()


def turn_of(req):
    """A request's messages from its last user message on: this turn only,
    not the earlier turns a session carries."""
    messages = req.get("messages") or []
    users = [i for i, m in enumerate(messages) if m.get("role") == "user"]
    return messages[users[-1]:] if users else []


def tool_results(dumps):
    """The tool answers this turn's model was shown, in order, from the last request."""
    return [str(m.get("content")) for m in (turn_of(dumps[-1]) if dumps else []) if m.get("role") == "tool"]


def offered(dumps):
    """Every tool name offered to the model in these requests."""
    return {t.get("function", {}).get("name") for req in dumps for t in req.get("tools") or []}


def citations_of(results):
    """`knowledge_id#chunk_id` → text of every evidence item the reads gave as
    a citation, with a revision."""
    found = {}
    for result in results:
        try:
            read = json.loads(result)
        except ValueError:
            continue
        if not isinstance(read, dict) or read.get("kind") != "knowledge.read":
            continue
        for item in read.get("evidence") or []:
            if item.get("use") == "citation" and item.get("content_tag") and item.get("chunk_id"):
                found[f"{read.get('knowledge_id')}#{item['chunk_id']}"] = str(item.get("text", ""))
    return found


def has_fact(text):
    return all(part in text for part in FACT_PARTS)


def evidence_of(result):
    """The hits of a search result; anything else has none."""
    try:
        found = json.loads(result)
    except ValueError:
        return []
    if not isinstance(found, dict) or found.get("kind") != "knowledge.search":
        return []
    return [h for h in found.get("evidence") or [] if isinstance(h, dict)]


def answer_of(run):
    return ((run or {}).get("output") or {}).get("text") or ""


def cases(d, lib, real):
    endpoint = {"endpoint": f"{WK}/api/v1", "knowledge_base_ids": [lib["kb"]]}
    s1, _, r1 = d.host("PUT", body=endpoint)
    s2, _, r2 = d.host("PUT", "/credential", {"kind": "api_key", "value": lib["key"], "expires_at": int(time.time()) + 3600})
    check("E0", "绑定按 main 的做法激活并推入 C", s1 == 204 and s2 == 204, f"PUT binding {s1} {r1[:80]}；PUT credential {s2} {r2[:80]}")

    DUMPS.clear()
    session = d.session()
    status, run, raw = d.ask(QUESTION if real else f"{MARKER} {QUESTION}", BINDING, session)
    results = tool_results(DUMPS)
    answer = answer_of(run)
    check("E1", "带绑定的主对话 run 完成", (run or {}).get("status") == "completed",
          f"创建 {status}，状态 {(run or {}).get('status')} {raw[:120]}")
    evidence = evidence_of(results[0] if results else "")
    check("E2", "检索：证据都来自样本库，并含完整的样本事实",
          bool(evidence) and all(h.get("knowledge_base_id") == lib["kb"] for h in evidence)
          and any(has_fact(h.get("snippet", "")) for h in evidence),
          f"{len(evidence)} 条证据；第一条 {json.dumps(evidence[:1], ensure_ascii=False)[:400]}")
    cited = citations_of(results)
    body, _, sources = answer.partition("来源")
    # The body is what precedes the source list, so the ids in it cannot stand in for the fact.
    check("E3", "读回为 citation 再作答：正文含样本事实（八点、林小满），「来源」列出的 knowledge_id#chunk_id 是读回的 citation",
          bool(cited) and any(c in sources for c in cited) and "八点" in body and "林小满" in body,
          f"citation {sorted(cited)}；回答 {answer[:400]!r}")
    if real:
        return

    DUMPS.clear()
    _, run, raw = d.ask(f"{MARKER} 第二轮", BINDING, session)
    again = evidence_of((tool_results(DUMPS) or [""])[0])
    check("E4", "同一会话的下一轮照样能检索（只看这一轮的工具结果）",
          (run or {}).get("status") == "completed" and any(has_fact(h.get("snippet", "")) for h in again),
          f"状态 {(run or {}).get('status')}，{len(again)} 条证据 {(tool_results(DUMPS) or [''])[0][:400]}")

    hit = evidence[0] if evidence else {}
    DUMPS.clear()
    _, run, raw = d.ask(f"M5-READ {hit.get('knowledge_id')} {hit.get('chunk_id')} {hit.get('content_tag')}", BINDING)
    try:
        read = json.loads(answer_of(run))
    except ValueError:
        read = {}
    read = read if isinstance(read, dict) else {}
    same = [t for t in read.get("evidence") or [] if isinstance(t, dict) and t.get("chunk_id") == hit.get("chunk_id")]
    check("E5", "回源：在新的一轮按引用读回，正是被引用的块（citation、修订一致、非空）且这一块含样本事实",
          bool(hit.get("content_tag")) and read.get("knowledge_id") == hit.get("knowledge_id")
          and any(t.get("use") == "citation" and t.get("content_tag") == hit.get("content_tag") and has_fact(t.get("text", ""))
                  for t in same),
          f"read {answer_of(run)[:400]!r}")

    DUMPS.clear()
    _, run, raw = d.ask("M5-TWICE", BINDING)
    twice = tool_results(DUMPS)
    check("E6", "同一个 run 里检索之后再检索一次照样能拿到证据（Knowledge 同源例外）",
          len(twice) == 2 and all(any(has_fact(h.get("snippet", "")) for h in evidence_of(r)) for r in twice),
          f"两次结果：{[r[:120] for r in twice]!r}")

    DUMPS.clear()
    _, run, raw = d.ask("M5-FETCH", BINDING)
    fetch = tool_results(DUMPS)
    check("N3", "同一个 run 里检索成功之后，http_fetch 仍被出口策略拒绝（例外只放行 Knowledge 自己）",
          len(fetch) == 2 and bool(evidence_of(fetch[0])) and "denied by policy" in fetch[1],
          f"结果：{[r[:120] for r in fetch]!r}")

    DUMPS.clear()
    _, run, raw = d.ask(f"{MARKER} {QUESTION}", None)
    tools = offered(DUMPS)
    check("N1", "不带绑定的 run 拿不到任何 Knowledge 工具",
          (run or {}).get("status") == "completed" and bool(DUMPS) and not tools & {"knowledge_search", "knowledge_read"},
          f"状态 {(run or {}).get('status')}，提供的工具里 Knowledge：{sorted(tools & {'knowledge_search', 'knowledge_read'})} {raw[:80]}")

    s, _, r = d.host("DELETE")
    status, run, raw = d.ask(f"{MARKER} {QUESTION}", BINDING)
    check("N2", "Off（DELETE 绑定）后，指名该绑定的新 run 在创建时被拒",
          run is None and status == 409 and "knowledge_binding_inactive" in raw,
          f"DELETE {s} {r[:60]}；创建 {status} {raw[:160]}")


def main():
    embed = model = d = None
    real = ARGS.model_url is not None
    try:
        embed = serve(FakeEmbed, 11434)
        lib = weknora_setup()
        print(f"WeKnora sample library ready: kb {lib['kb']}, document {lib['doc']}", flush=True)
        model = serve(Recorder if real else Stub)
        url = f"http://127.0.0.1:{model.server_address[1]}"
        model_env = {"OLLAMA_URL": url, "DEFAULT_MODEL": ARGS.model} if real else {"OMLX_URL": url, "DEFAULT_MODEL": STUB_MODEL}
        d = Daemon(os.path.join(WORK, "data"), model_env)
        cases(d, lib, real)
    finally:
        if d is not None:
            d.stop()
        for server in (model, embed):
            if server is not None:
                server.shutdown()
                server.server_close()
    with open(os.path.join(WORK, "report.md"), "w") as f:
        mode = f"真实模型 {ARGS.model}（经 {ARGS.model_url}）" if real else "脚本模型（stub）"
        f.write(f"# KB-M5 端到端验收\n\n- WeKnora：{WK}\n- 模型：{mode}\n- 时间（UTC）：{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n\n")
        f.write("| 编号 | 检查 | 结果 | 证据 |\n|---|---|---|---|\n")
        for cid, what, ok, ev in RESULTS:
            f.write(f"| {cid} | {what} | {'PASS' if ok else 'FAIL'} | {ev.replace('|', '\\|').replace(chr(10), ' ')} |\n")
    print(f"report: {os.path.join(WORK, 'report.md')}")
    return 0 if all(ok for _, _, ok, _ in RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
