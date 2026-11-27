import json, os, sys, uuid, time, urllib.request, urllib.error
D = json.load(open(os.path.expanduser("~/weknora-demo-stack/demo-credentials.json")))
KEY = D["retrieve_key"]
KBS = ["ffec8cdb-0bad-45a5-8c30-9091e425e0e1", "439127c1-8157-4095-997d-120a87ed83b7"]
def search(q, kbs=None, n=5):
    body = {"protocol_version": 1, "request_id": "c3e-" + uuid.uuid4().hex[:8], "generation": 1,
            "query": q, "scope": {"knowledge_base_ids": kbs or KBS},
            "budget": {"deadline_ms": 15000, "max_results": n, "max_concurrency": 1}}
    req = urllib.request.Request("http://127.0.0.1:18282/api/v1/agent24/knowledge/search", json.dumps(body).encode(),
                                 {"X-API-Key": KEY, "Content-Type": "application/json"}, method="POST")
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=60) as r: st, raw = r.status, r.read()
    except urllib.error.HTTPError as e: st, raw = e.code, e.read()
    return st, json.loads(raw), time.time() - t
def search_all(q, n=5):
    """R3.7(b) like #2058: on mixed_score_space ask each group alone, round-robin by rank, dedupe, cut to n."""
    st, j, dt = search(q, KBS, n)
    if st == 200: return j.get("hits", []), dt, 1
    groups = [g["knowledge_base_ids"] for g in j["error"]["details"]["groups"]]
    answers, calls = [], 1
    for g in groups:
        st2, j2, dt2 = search(q, g, n); dt += dt2; calls += 1
        answers.append(j2.get("hits", []) if st2 == 200 else [])
    out, seen = [], set()
    for i in range(n):
        for a in answers:
            if i < len(a):
                h = a[i]; k = (h.get("knowledge_id"), h.get("chunk_id"))
                if k not in seen: seen.add(k); out.append(h)
    return out[:n], dt, calls
if __name__ == "__main__":
    st, j, dt = search(sys.argv[1], [sys.argv[2]] if len(sys.argv) > 2 else None)
    print(st, round(dt,2), {k: v for k, v in j.items() if k != "hits"})
    for h in j.get("hits", []):
        print(json.dumps({k: (v[:300] if isinstance(v, str) else v) for k, v in h.items()}, ensure_ascii=False))
