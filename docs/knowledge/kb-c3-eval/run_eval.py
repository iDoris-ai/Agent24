"""KB-C3-EVAL: simple retrieval (A) vs rewrite + multi-step retrieval + evidence organizing (B).
Retrieval: demo stack 18282 (same endpoint/key as the App), grouped by score space like #2058.
Model: Qwen3-8B-4bit via mlx_lm.server on 127.0.0.1:18095, thinking off (/no_think), temperature 0.
Writes results.jsonl (one line per question x mode). No credentials are printed."""
import json, os, re, sys, time, urllib.request
from search import search_all

LLM = "http://127.0.0.1:18095/v1/chat/completions"
MODEL = "/Volumes/UltraDisk/Dev2/caches/omlx-models/mlx-community/Qwen3-8B-4bit"
DOC = {"c597dfe1": "s01-01", "17b1f6d6": "s01-02", "8f110fb4": "s01-03", "5f46261b": "s01-04",
       "a26e3619": "s01-05", "3ce832e6": "s01-07", "38dc6813": "s01-08", "50f957f8": "k2"}
SNIP = 600          # chars per snippet, both modes
TOPK = 5            # A: one search, top 5
SUB_K = 4           # B: per sub-query top 4
MAX_SUB = 3         # B: at most 3 sub-queries (hard cap in the harness)
SYS = "你是知识库问答助手。只根据给出的资料回答，资料里没有的就说没找到，不要编造。回答用中文，引用写成[编号]。/no_think"


def chat(messages, max_tokens=700):
    body = {"model": MODEL, "messages": messages, "temperature": 0.0, "max_tokens": max_tokens}
    req = urllib.request.Request(LLM, json.dumps(body).encode(), {"Content-Type": "application/json"})
    t = time.time()
    with urllib.request.urlopen(req, timeout=600) as r:
        j = json.loads(r.read())
    dt = time.time() - t
    text = j["choices"][0]["message"]["content"]
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    u = j.get("usage", {})
    return text, {"prompt": u.get("prompt_tokens", 0), "completion": u.get("completion_tokens", 0), "llm_s": dt}


def fmt(hits, start=1):
    out = []
    for i, h in enumerate(hits, start):
        title = (h.get("display") or {}).get("title", "?")
        out.append(f"[{i}] 《{title}》\n{h['snippet']['text'][:SNIP]}")
    return "\n\n".join(out)


def docs_of(hits):
    return sorted({DOC.get(h["knowledge_id"][:8], h["knowledge_id"][:8]) for h in hits})


def mode_a(q):
    t0 = time.time()
    hits, sdt, calls = search_all(q, TOPK)
    ans, u = chat([{"role": "system", "content": SYS},
                   {"role": "user", "content": f"资料：\n{fmt(hits)}\n\n问题：{q}"}])
    return {"answer": ans, "docs": docs_of(hits), "search_calls": calls, "search_s": sdt,
            "llm_calls": 1, "prompt": u["prompt"], "completion": u["completion"],
            "max_prompt": u["prompt"], "total_s": time.time() - t0, "steps": []}


def mode_b(q):
    t0 = time.time(); steps = []; P = C = 0; maxp = 0; sdt_all = 0; scalls = 0; llm = 0
    def acc(u):
        nonlocal P, C, maxp, llm
        P += u["prompt"]; C += u["completion"]; maxp = max(maxp, u["prompt"]); llm += 1
    # 1. plan: rewrite into <= 3 sub-queries
    plan, u = chat([{"role": "system", "content": "把用户问题拆成最多3个适合在知识库里检索的短查询，每个查询只问一件事，保留原文里的关键词（地名、年份、文件名、术语）。只输出 JSON 数组，例如 [\"查询1\",\"查询2\"]。/no_think"},
                    {"role": "user", "content": q}], max_tokens=200)
    acc(u)
    m = re.search(r"\[.*\]", plan, re.S)
    try:
        raw = json.loads(m.group(0)) if m else []
        subs = []
        for x in raw:  # an 8B model often wraps each query in an object; accept its values
            for v in (x.values() if isinstance(x, dict) else [x]):
                if isinstance(v, str) and v.strip(): subs.append(v.strip())
    except ValueError:
        subs = []
    plan_ok = bool(subs)
    subs = (subs or [q])[:MAX_SUB]
    steps.append({"step": "plan", "ok": plan_ok, "subs": subs, "raw": plan[:300]})
    # 2+3. search each sub-query, organize its evidence into cited facts
    seen = set(); all_hits = []; notes = []; n = 1
    for s in subs:
        hits, sdt, calls = search_all(s, SUB_K); sdt_all += sdt; scalls += calls
        hits = [h for h in hits if (h["knowledge_id"], h["chunk_id"]) not in seen]
        for h in hits: seen.add((h["knowledge_id"], h["chunk_id"]))
        if not hits:
            steps.append({"step": "search", "sub": s, "new_hits": 0}); continue
        all_hits += hits
        facts, u = chat([{"role": "system", "content": "从资料里摘出与问题有关的事实，每条一行，原样保留数字、日期、时间，末尾标注来源编号如[3]。与问题无关的不要写。资料里没有就输出「无」。/no_think"},
                         {"role": "user", "content": f"问题：{q}\n本步查询：{s}\n\n资料：\n{fmt(hits, n)}"}], max_tokens=400)
        acc(u); n += len(hits)
        notes.append(f"（查询：{s}）\n{facts}")
        steps.append({"step": "search+organize", "sub": s, "new_hits": len(hits), "docs": docs_of(hits)})
    # 4. answer from organized evidence
    src = "\n".join(f"[{i}] 《{(h.get('display') or {}).get('title','?')}》" for i, h in enumerate(all_hits, 1))
    ans, u = chat([{"role": "system", "content": SYS},
                   {"role": "user", "content": f"整理好的证据：\n" + "\n\n".join(notes) + f"\n\n来源列表：\n{src}\n\n问题：{q}"}])
    acc(u)
    return {"answer": ans, "docs": docs_of(all_hits), "search_calls": scalls, "search_s": sdt_all,
            "llm_calls": llm, "prompt": P, "completion": C, "max_prompt": maxp,
            "total_s": time.time() - t0, "steps": steps}


MAX_EVID = 8        # C: cap on raw chunks put into the one answer prompt


def mode_c(q):
    """Rewrite + multi-step retrieval, no organize step: raw evidence (<= MAX_EVID chunks) into one answer."""
    t0 = time.time(); P = C = maxp = 0; sdt_all = 0; scalls = 0
    plan, u = chat([{"role": "system", "content": "把用户问题拆成最多3个适合在知识库里检索的短查询，每个查询只问一件事，保留原文里的关键词（地名、年份、文件名、术语）。只输出 JSON 数组，例如 [\"查询1\",\"查询2\"]。/no_think"},
                    {"role": "user", "content": q}], max_tokens=200)
    P += u["prompt"]; C += u["completion"]; maxp = u["prompt"]
    m = re.search(r"\[.*\]", plan, re.S); subs = []
    try:
        for x in (json.loads(m.group(0)) if m else []):
            for v in (x.values() if isinstance(x, dict) else [x]):
                if isinstance(v, str) and v.strip(): subs.append(v.strip())
    except ValueError:
        pass
    plan_ok = bool(subs); subs = (subs or [q])[:MAX_SUB]
    per = []
    for s in [q] + subs:  # the original question is searched too (its top 4; A takes top 5)
        hits, sdt, calls = search_all(s, SUB_K); sdt_all += sdt; scalls += calls; per.append(hits)
    seen = set(); evid = []
    for i in range(SUB_K):  # round-robin across queries, dedupe, cap
        for hs in per:
            if i < len(hs):
                h = hs[i]; k = (h["knowledge_id"], h["chunk_id"])
                if k not in seen and len(evid) < MAX_EVID: seen.add(k); evid.append(h)
    ans, u = chat([{"role": "system", "content": SYS},
                   {"role": "user", "content": f"资料：\n{fmt(evid)}\n\n问题：{q}"}])
    P += u["prompt"]; C += u["completion"]; maxp = max(maxp, u["prompt"])
    return {"answer": ans, "docs": docs_of(evid), "search_calls": scalls, "search_s": sdt_all,
            "llm_calls": 2, "prompt": P, "completion": C, "max_prompt": maxp,
            "total_s": time.time() - t0, "steps": [{"step": "plan", "ok": plan_ok, "subs": subs}]}


def score(ans, gold):
    a = ans.replace(" ", "").lower()
    hit = [any(g.replace(" ", "").lower() in a for g in alts) for alts in gold]
    return sum(hit), len(gold), hit


if __name__ == "__main__":
    qs = json.load(open("questions.json"))
    MODES = os.environ.get("MODES", "AB")
    only = set(sys.argv[1:])
    with open("results.jsonl", "a") as f:
        for item in qs:
            if only and item["id"] not in only: continue
            for name, fn in [m for m in (("A", mode_a), ("B", mode_b), ("C", mode_c)) if m[0] in MODES]:
                r = fn(item["q"]); got, tot, hit = score(r["answer"], item["gold"])
                r.update(id=item["id"], mode=name, gold_hit=got, gold_total=tot, gold_flags=hit,
                         doc_recall=f"{len(set(item['docs']) & set(r['docs']))}/{len(item['docs'])}")
                f.write(json.dumps(r, ensure_ascii=False) + "\n"); f.flush()
                print(item["id"], name, f"{got}/{tot}", r["doc_recall"], f"{r['total_s']:.1f}s",
                      f"p={r['prompt']} c={r['completion']} maxp={r['max_prompt']}", flush=True)
