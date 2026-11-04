#!/usr/bin/env python3
"""生成 docs/tracking/<日期>/PROGRESS.md §1「本期合入的 PR」表。

用法（在仓库根目录）：
    python3 scripts/gen-tracking-merges.py --since 2026-10-22 [--until HEAD]

数据来源：`git log --first-parent --merges --since=<日期> <until>`，即合入当前分支主线的
合并提交（GitHub 的 "Merge pull request #N from org/branch"）。一句话取合并提交正文的第一行
（GitHub 把 PR 标题放在这里）。只用 Python 3 标准库和本地 git，不访问网络。
"""
import argparse
import re
import subprocess

ap = argparse.ArgumentParser()
ap.add_argument("--since", required=True)
ap.add_argument("--until", default="HEAD")
a = ap.parse_args()

SEP = "\x1f"
REC = "\x1e"
out = subprocess.run(
    ["git", "log", "--first-parent", "--merges", f"--since={a.since}", a.until,
     f"--format=%h{SEP}%ad{SEP}%s{SEP}%b{REC}", "--date=short"],
    check=True, capture_output=True, text=True).stdout
rows = []
for rec in out.split(REC):
    rec = rec.strip("\n")
    if not rec:
        continue
    sha, date, subj, body = rec.split(SEP, 3)
    m = re.match(r"Merge pull request #(\d+) from [^/]+/(.+)", subj)
    if m:
        pr, branch = "#" + m.group(1), m.group(2)
    else:
        pr, branch = "（无 PR 号）", subj.replace("|", "/")
    title = (body.strip().splitlines() or [""])[0].replace("|", "/")
    if len(title) > 110:
        title = title[:110] + "…"
    rows.append((pr, date, branch, title, sha))
print("| PR | 日期 | 分支（前缀即方向） | 一句话（PR 标题） | 合并提交 |")
print("|---|---|---|---|---|")
for pr, date, branch, title, sha in rows:
    print(f"| {pr} | {date} | `{branch}` | {title} | `{sha}` |")
print()
print(f"共 {len(rows)} 个合并（`--since={a.since}`）。")
