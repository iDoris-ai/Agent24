#!/usr/bin/env python3
"""List every path+method in protocol/openapi.yaml with tags, grouped by prefix.
Usage: python3 -I openapi_ops.py <repo>/protocol/openapi.yaml [--tsv]"""
import sys, re, yaml, collections

METHODS = {"get", "put", "post", "delete", "patch", "head", "options", "trace"}

def ops(path):
    doc = yaml.safe_load(open(path, encoding="utf-8"))
    # line numbers of path keys
    lines = open(path, encoding="utf-8").read().splitlines()
    lineno = {}
    in_paths = False
    for i, l in enumerate(lines, 1):
        if l.startswith("paths:"):
            in_paths = True
            continue
        if in_paths and re.match(r"^\S", l):
            in_paths = False
        m = re.match(r"^  (/[^:]*):\s*$", l) if in_paths else None
        if m:
            lineno[m.group(1).strip("'\"")] = i
    out = []
    for p, item in (doc.get("paths") or {}).items():
        for m, op in item.items():
            if m in METHODS:
                out.append((p, m.upper(), ",".join(op.get("tags", []) or []),
                            op.get("operationId", ""), lineno.get(p, "?")))
    return doc, out

def prefix(p):
    segs = p.strip("/").split("/")
    if segs[:2] == ["api", "v1"] and len(segs) > 2:
        return "/api/v1/" + segs[2]
    return "/" + "/".join(segs[:2])

if __name__ == "__main__":
    doc, out = ops(sys.argv[1])
    if "--tsv" in sys.argv:
        for r in out:
            print("\t".join(map(str, r)))
        sys.exit()
    print("top-level tags:", [t.get("name") for t in doc.get("tags", []) or []])
    print("servers:", doc.get("servers"))
    print("paths:", len(doc.get("paths", {})), "operations:", len(out))
    g = collections.defaultdict(list)
    for r in out:
        g[prefix(r[0])].append(r)
    for k in sorted(g):
        print(f"\n## {k} ({len(g[k])})")
        for p, m, t, oid, ln in g[k]:
            print(f"  {m:6} {p}  tags=[{t}] opId={oid} L{ln}")
