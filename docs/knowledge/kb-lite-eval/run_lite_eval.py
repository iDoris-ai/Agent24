#!/usr/bin/env python3
"""KB-LITE-01 实测：拉起托管档 Lite，计时、采样 RSS、导入文档、跑负例。

用法（python3 -I 运行，只用标准库）：
  python3 -I run_lite_eval.py --pkg <解压后的制品目录> --work <一次性工作目录> \
      --docs <md/txt 目录> --embed-base <OpenAI 兼容 base_url，如 http://127.0.0.1:18911/v1> \
      --embed-model bge-m3:latest [--embed-key-file <文件>] [--dim 1024] [--runs 3]

输出：每一步一行 JSON（stdout），最后一行 {"summary": ...}。
凭据（setup_token、JWT、embedding key）只在内存里，不写进输出。
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid


def out(**kv):
    print(json.dumps(kv, ensure_ascii=False), flush=True)


def rss_kb(pid):
    r = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True)
    s = r.stdout.strip()
    return int(s) if s else 0


class Sampler(threading.Thread):
    def __init__(self, pid, interval=0.25):
        super().__init__(daemon=True)
        self.pid, self.interval, self.peak, self.last, self.stop_flag = pid, interval, 0, 0, False

    def run(self):
        while not self.stop_flag:
            v = rss_kb(self.pid)
            if v == 0:
                return
            self.last = v
            self.peak = max(self.peak, v)
            time.sleep(self.interval)


def write_env(work, args):
    data = os.path.join(work, "data")
    os.makedirs(data, exist_ok=True)
    env_path = os.path.join(work, "lite.env")
    lines = [
        "GIN_MODE=release",
        "LOG_LEVEL=info",
        f"LOG_PATH={work}/logs",
        "DB_DRIVER=sqlite",
        f"DB_PATH={data}/weknora.db",
        "RETRIEVE_DRIVER=sqlite",
        "STORAGE_TYPE=local",
        f"LOCAL_STORAGE_BASE_DIR={data}/files",
        "STREAM_MANAGER_TYPE=memory",
        "NEO4J_ENABLE=false",
        "WEKNORA_SANDBOX_MODE=disabled",
        "ENABLE_GRAPH_RAG=false",
        "CONCURRENCY_POOL_SIZE=3",
        f"BUILTIN_MODELS_CONFIG={work}/builtin_models.yaml",
    ]
    if args.ssrf_whitelist:
        lines.append(f"SSRF_WHITELIST={args.ssrf_whitelist}")
    with open(env_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    key = ""
    if args.embed_key_file:
        with open(args.embed_key_file) as f:
            key = f.read().strip()
    yaml = f"""builtin_models:
  - id: builtin-embedding-lite-eval
    type: Embedding
    source: remote
    is_default: true
    name: {args.embed_model}
    parameters:
      base_url: {args.embed_base}
      api_key: ${{LITE_EVAL_EMBED_KEY}}
      provider: generic
      embedding_parameters:
        dimension: {args.dim}
        truncate_prompt_tokens: 0
"""
    with open(os.path.join(work, "builtin_models.yaml"), "w") as f:
        f.write(yaml)
    return env_path, key


def launch(pkg, work, env_path, key, port=0):
    r, w = os.pipe()
    env = {k: v for k, v in os.environ.items() if k not in ("JWT_SECRET", "SYSTEM_AES_KEY", "JIEBA_DICT_DIR")}
    env["ENV_FILE"] = env_path
    env["LITE_EVAL_EMBED_KEY"] = key
    logf = open(os.path.join(work, "stdout-stderr.log"), "ab")
    t0 = time.monotonic()
    p = subprocess.Popen(
        [os.path.join(pkg, "WeKnora-lite"), "--managed", "--state-dir", os.path.join(work, "state"),
         "--port", str(port), "--handshake-fd", "3"],
        cwd=pkg, env=env, stdout=logf, stderr=logf, stdin=subprocess.DEVNULL,
        pass_fds=(3,), preexec_fn=lambda: os.dup2(w, 3) if w != 3 else None,
    )
    os.close(w)
    return p, os.fdopen(r, "r"), t0


def read_line(fh, timeout):
    res = {}

    def rd():
        res["line"] = fh.readline()

    th = threading.Thread(target=rd, daemon=True)
    th.start()
    th.join(timeout)
    line = res.get("line")
    if not line:
        raise RuntimeError("handshake line timeout or EOF")
    return json.loads(line)


def http(method, url, body=None, headers=None, timeout=60):
    data = None
    hdr = dict(headers or {})
    if body is not None and not isinstance(body, bytes):
        data = json.dumps(body).encode()
        hdr["Content-Type"] = "application/json"
    elif isinstance(body, bytes):
        data = body
    req = urllib.request.Request(url, data=data, method=method, headers=hdr)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"{}")
        except ValueError:
            return e.code, {"raw": raw[:200].decode(errors="replace")}


def multipart(path):
    boundary = uuid.uuid4().hex
    name = os.path.basename(path)
    with open(path, "rb") as f:
        content = f.read()
    ctype = "text/markdown" if name.endswith(".md") else "text/plain"
    body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
            f"Content-Type: {ctype}\r\n\r\n").encode() + content + f"\r\n--{boundary}--\r\n".encode()
    return body, f"multipart/form-data; boundary={boundary}"


def stop(p):
    p.terminate()
    try:
        p.wait(30)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()


def non_loopback_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pkg", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--docs", required=True)
    ap.add_argument("--embed-base", required=True)
    ap.add_argument("--embed-model", required=True)
    ap.add_argument("--embed-key-file")
    ap.add_argument("--dim", type=int, default=1024)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--ssrf-whitelist", default="127.0.0.1",
                    help="loopback model endpoints are refused by WeKnora's SSRF check unless whitelisted")
    ap.add_argument("--idle-seconds", type=int, default=20)
    args = ap.parse_args()
    if os.path.exists(args.work):
        sys.exit("work dir must not exist (one-shot)")
    os.makedirs(args.work)
    env_path, key = write_env(args.work, args)
    summary = {"cold_start": []}

    # 1. 冷启动若干次（第一次含建库迁移），每次都是新进程
    secrets_seen = None
    for i in range(args.runs):
        p, hs, t0 = launch(args.pkg, args.work, env_path, key)
        ready = read_line(hs, 120)
        t_proc = time.monotonic() - t0
        models = read_line(hs, 60)
        t_models = time.monotonic() - t0
        origin = ready["origin"]
        st, _ = http("GET", origin + "/health")
        t_health = time.monotonic() - t0
        sampler = Sampler(p.pid)
        sampler.start()
        time.sleep(args.idle_seconds)
        idle = sampler.last
        # 负例：外网地址连不上
        ip = non_loopback_ip()
        ext = None
        if ip:
            port = int(origin.rsplit(":", 1)[1])
            try:
                socket.create_connection((ip, port), timeout=1).close()
                ext = "connected"
            except OSError:
                ext = "refused"
        # 负例：注册入口关闭
        rs, rb = http("POST", origin + "/api/v1/auth/register",
                      {"username": "eve", "email": "eve@example.com", "password": "Passw0rd!x"})
        with open(os.path.join(args.work, "state", "managed-secrets.json")) as f:
            sec = f.read()
        same = secrets_seen is None or secrets_seen == sec
        secrets_seen = sec
        out(step="cold_start", run=i + 1, process_ready_s=round(t_proc, 2), models_line_s=round(t_models, 2),
            health_status=st, health_s=round(t_health, 2), origin_host=origin.split("//")[1].split(":")[0],
            models=models, idle_rss_mb=round(idle / 1024, 1), external_ip_probe=ext,
            register_status=rs, register_body=rb.get("error", rb) if isinstance(rb, dict) else rb,
            secrets_unchanged_since_first=same)
        summary["cold_start"].append({"process_ready_s": round(t_proc, 2), "idle_rss_mb": round(idle / 1024, 1)})
        if i < args.runs - 1:
            sampler.stop_flag = True
            stop(p)
    # 2. 最后一个进程上：auto-setup 拿 JWT，建库，导入文档，采峰值
    token = ready["setup_token"]
    st, body = http("POST", origin + "/api/v1/auth/auto-setup", {}, {"X-WeKnora-Desktop-Token": token})
    jwt = (body.get("data") or body).get("token") if isinstance(body, dict) else None
    out(step="auto_setup", status=st, got_jwt=bool(jwt))
    if not jwt:
        out(step="auto_setup_body_keys", keys=list(body.keys()) if isinstance(body, dict) else str(body)[:200])
        stop(p)
        return
    auth = {"Authorization": "Bearer " + jwt}
    st, body = http("POST", origin + "/api/v1/knowledge-bases",
                    {"name": "lite-eval", "description": "KB-LITE-01 eval", "type": "document",
                     "embedding_model_id": "builtin-embedding-lite-eval"}, auth)
    kb = (body.get("data") or {}).get("id") if isinstance(body, dict) else None
    out(step="kb_create", status=st, kb=bool(kb), err=None if kb else body)
    if not kb:
        stop(p)
        return
    files = sorted(os.listdir(args.docs))
    base_peak = sampler.peak
    sampler.peak = 0
    t_imp = time.monotonic()
    ids = []
    for name in files:
        mb, ctype = multipart(os.path.join(args.docs, name))
        st, body = http("POST", f"{origin}/api/v1/knowledge-bases/{kb}/knowledge/file", mb,
                        dict(auth, **{"Content-Type": ctype}), timeout=120)
        kid = (body.get("data") or {}).get("id") if isinstance(body, dict) else None
        if kid:
            ids.append(kid)
        else:
            out(step="upload_fail", file=name, status=st, body=body)
    # 等全部解析/向量化完成
    deadline = time.monotonic() + 900
    done = {}
    while time.monotonic() < deadline:
        st, body = http("GET", f"{origin}/api/v1/knowledge-bases/{kb}/knowledge?page=1&page_size=100", None, auth)
        items = (body.get("data") or []) if isinstance(body, dict) else []
        done = {}
        for it in items:
            done[it.get("parse_status")] = done.get(it.get("parse_status"), 0) + 1
        if items and all(it.get("parse_status") in ("completed", "failed") for it in items) and len(items) >= len(ids):
            break
        time.sleep(1)
    t_import = time.monotonic() - t_imp
    time.sleep(2)
    peak = sampler.peak
    # 检索一次，确认 embedding 真的走通
    # 只走向量（关掉关键词），命中即说明 query embedding 也经网关算出
    st, body = http("POST", f"{origin}/api/v1/knowledge-bases/{kb}/hybrid-search",
                    {"query_text": "深度检索评估的结论是什么", "match_count": 3,
                     "disable_keywords_match": True}, auth)
    hits = len(body.get("data") or []) if isinstance(body, dict) and isinstance(body.get("data"), list) else None
    sampler.stop_flag = True
    out(step="import", files=len(files), uploaded=len(ids), status_counts=done, import_s=round(t_import, 1),
        peak_rss_mb=round(peak / 1024, 1), before_import_peak_mb=round(base_peak / 1024, 1),
        search_status=st, search_hits=hits)
    stop(p)
    # 3. 日志里不应出现 setup token 和 JWT
    leaked = []
    for root, _, fs in os.walk(args.work):
        for fn in fs:
            if fn.endswith((".db", ".db-wal", ".db-shm")) or "files" in root.split(os.sep):
                continue
            fp = os.path.join(root, fn)
            try:
                with open(fp, "rb") as f:
                    raw = f.read()
            except OSError:
                continue
            if token.encode() in raw or jwt.encode() in raw:
                leaked.append(os.path.relpath(fp, args.work))
    out(step="token_in_logs", leaked_files=leaked)
    summary.update(import_s=round(t_import, 1), peak_rss_mb=round(peak / 1024, 1), search_hits=hits)
    out(summary=summary)


if __name__ == "__main__":
    main()
