#!/usr/bin/env python3
"""Extract axum `.route("<path>", <method chain>)` registrations (production fns only)
and diff them against protocol/openapi.yaml (method-level) and route_policy.rs.
Usage: python3 -I rust_routes.py <repo>"""
import sys, re, os, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import openapi_ops  # noqa: E402

ROOT = sys.argv[1]
R = lambda p: os.path.join(ROOT, p)

# (file, function anchor, path prefix applied, crate/module label)
SOURCES = [
    ("rust/apps/agent24d/src/server.rs", "pub fn build_router_with_modules", "", "agent24d kernel"),
    ("rust/apps/agent24d/src/docked_router.rs", "pub fn docked_router", "", "agent24d docked"),
    ("rust/crates/agent24-comm/src/router.rs", "pub fn router(", "/api/v1/comm", "agent24-comm"),
    ("rust/crates/agent24-comm/src/groupchat.rs", "pub(crate) fn routes(", "/api/v1/comm", "agent24-comm"),
    ("rust/apps/agent24d/src/comm_routes.rs", "fn runtime_router(", "/api/v1/comm", "agent24d comm_routes"),
    ("rust/apps/agent24-documents/src/lib.rs", "pub fn router(", "/api/v1/documents", "agent24-documents (OOP module)"),
]
METHOD_RE = re.compile(r"(?:^|[\s.(:])(get|post|put|patch|delete|head|options|any)\s*\(")

def balanced(s, i):
    """s[i] == '(' ; return index of matching ')'."""
    depth, j, instr = 0, i, False
    while j < len(s):
        c = s[j]
        if instr:
            if c == "\\": j += 1
            elif c == '"': instr = False
        elif c == '"': instr = True
        elif c == "(": depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0: return j
        j += 1
    raise ValueError("unbalanced")

def strip_comments(s):
    return re.sub(r"//[^\n]*", lambda m: " " * len(m.group(0)), s)

def extract(path, anchor, prefix, label):
    src = open(R(path), encoding="utf-8").read()
    start = src.index(anchor)
    end = src.index("\n}\n", start)
    body = strip_comments(src[start:end])
    out = []
    for m in re.finditer(r"\.route\(", body):
        op = m.end() - 1
        cl = balanced(body, op)
        inner = body[op + 1:cl]
        pm = re.search(r'"([^"]*)"', inner)
        if not pm: continue
        rest = inner[pm.end():]
        # handlers: method(handler) — capture handler expr
        for mm in METHOD_RE.finditer(rest):
            ho = pm.end() + mm.end() - 1
            hc = balanced(inner, ho)
            handler = re.sub(r"\s+", "", inner[ho + 1:hc])
            line = src[:start + m.start()].count("\n") + 1
            out.append((mm.group(1).upper(), prefix + pm.group(1), f"{path}:{line}", handler, label))
    return out

routes = []
for s in SOURCES:
    routes.extend(extract(*s))

# openapi
_, oas = openapi_ops.ops(R("protocol/openapi.yaml"))
oa = {(m, "/api/v1" + p): (t, oid, ln) for p, m, t, oid, ln in oas}

# route_policy
pol_src = open(R("rust/apps/agent24d/src/capabilities/route_policy.rs"), encoding="utf-8").read()
policy = {}
for i, l in enumerate(pol_src.splitlines(), 1):
    m = re.match(r'\s*row\("(\w+)", "([^"]+)", Operation::(\w+), AssuranceLevel::(\w+)\)', l)
    if m: policy[(m.group(1), m.group(2))] = (m.group(3), m.group(4), i)

code = collections.OrderedDict()
for meth, p, loc, h, label in routes:
    code.setdefault((meth, p), []).append((loc, h, label))

mode = sys.argv[2] if len(sys.argv) > 2 else "all"
if mode in ("all", "table"):
    print("| # | method | route | openapi tag / opId | owner file:line | handler | crate/module | policy |")
    print("|---|---|---|---|---|---|---|---|")
    keys = sorted(set(code) | set(oa), key=lambda k: (k[1], k[0]))
    for n, k in enumerate(keys, 1):
        t = oa.get(k)
        c = code.get(k, [])
        pol = policy.get(k)
        print(f"| {n} | {k[0]} | `{k[1]}` | {t[0]+' / '+t[1]+' (L'+str(t[2])+')' if t else '—(not in openapi)'} | "
              f"{'<br>'.join(x[0] for x in c) or '—(no literal route)'} | {'<br>'.join('`'+x[1]+'`' for x in c)} | "
              f"{'<br>'.join(x[2] for x in c)} | {pol[0]+'/'+pol[1]+' L'+str(pol[2]) if pol else '—'} |")
if mode in ("all", "diff"):
    print("\n## code routes:", len(code), " openapi ops:", len(oa), " policy rows:", len(policy))
    print("\n## A) openapi op, no literal code route")
    for k in sorted(set(oa) - set(code), key=lambda k: k[1]): print(" ", k)
    print("\n## B) code route, not in openapi (method+path)")
    for k in sorted(set(code) - set(oa), key=lambda k: k[1]): print(" ", k, code[k][0][0])
    print("\n## dup registrations (same method+path in >1 place)")
    for k, v in code.items():
        if len(v) > 1: print(" ", k, [x[0] for x in v])
    print("\n## P1) code route not in route_policy")
    for k in sorted(set(code) - set(policy), key=lambda k: k[1]): print(" ", k, code[k][0][0])
    print("\n## P2) policy row with no code route")
    for k in sorted(set(policy) - set(code), key=lambda k: k[1]): print(" ", k, "route_policy.rs:%d" % policy[k][2])
    print("\n## path-level: openapi paths whose method set differs from code")
    op = collections.defaultdict(set); cp = collections.defaultdict(set)
    for m, p in oa: op[p].add(m)
    for m, p in code: cp[p].add(m)
    for p in sorted(set(op) & set(cp)):
        if op[p] != cp[p]: print(f"  {p}: openapi={sorted(op[p])} code={sorted(cp[p])}")
