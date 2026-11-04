#!/usr/bin/env python3
"""由 `cargo metadata` 生成 docs/ARCHITECTURE-LAYERS.md §1.1 的依赖表。

用法（在仓库根目录）：
    python3 scripts/gen-layer-deps.py            # 把表打印到标准输出
    python3 scripts/gen-layer-deps.py --write    # 重写文档里两个标记之间的内容
    python3 scripts/gen-layer-deps.py --check    # 文档与代码不一致时退出码为 1

数据来源：`cargo metadata --no-deps --format-version 1 --manifest-path rust/Cargo.toml`。
只计工作区内部（agent24*）的依赖；"正常依赖"指 kind 为空的依赖，"仅 dev"指只出现在
dev-dependencies 里的依赖。crate 自己依赖自己（测试用的 feature 开关）不计。

crate 归层写在下面的 LAYER 表里，这是人工判断，不是 cargo 能算出来的。
新增 crate 如果没有出现在 LAYER 里，脚本会报错，逼着补归层。
cargo 可用 CARGO_TARGET_DIR 指到仓库外；本脚本只读 metadata，不会构建。
"""
import json
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOC = os.path.join(REPO, "docs", "ARCHITECTURE-LAYERS.md")
BEGIN = "<!-- BEGIN GENERATED: layer-deps (scripts/gen-layer-deps.py) -->"
END = "<!-- END GENERATED: layer-deps -->"

# crate（不带 agent24- 前缀）-> 层。层的定义见 ARCHITECTURE-LAYERS.md §1。
LAYER = {
    "protocol": "L0",
    "domain": "L0",
    "sidecar-host-protocol": "L0",
    "core": "L1",
    "store": "L2",
    "workspace": "L2",
    "os-cwd": "L2",
    "os-proc": "L2（平台原语）",
    "os-sqlite": "L2（平台原语）",
    "models": "L3",
    "tools": "L3",
    "policy": "L3",
    "memory": "L3",
    "scheduler": "L3",
    "mcp": "L3",
    "comm": "L3（例外：自带 REST 面）",
    "hw": "L3",
    "decide": "L3",
    "components": "L3",
    "idoris": "L3",
    "knowledge": "L3",
    "worker": "L3",
    "documents-client": "L3（客户端库）",
    "agent": "L4",
    "os-proto": "L4½",
    "os-fd": "L4½",
    "os-packages": "L4½",
    "os-sdk": "L4½",
    "agent24d": "L5",
    "documents": "L6",
    "sidecar-host": "L6",
    "cli": "L7",
}

ORDER = ["L0", "L1", "L2", "L3", "L4", "L4½", "L5", "L6", "L7"]


def short(name):
    """agent24-foo -> foo；agent24d 保持原样。"""
    return name[len("agent24-"):] if name.startswith("agent24-") else name


def layer_key(name):
    layer = LAYER[name]
    base = layer.split("（")[0]
    return (ORDER.index(base), layer, name)


def load():
    env = dict(os.environ)
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1",
         "--manifest-path", os.path.join(REPO, "rust", "Cargo.toml")],
        check=True, capture_output=True, text=True, env=env,
    ).stdout
    return json.loads(out)


def build_rows(meta):
    names = {p["name"] for p in meta["packages"]}
    rows = []
    for p in meta["packages"]:
        me = p["name"]
        normal, dev = set(), set()
        for d in p["dependencies"]:
            n = d["name"]
            if n == me or n not in names:
                continue
            (dev if d["kind"] == "dev" else normal).add(n)
        dev -= normal
        key = short(me)
        if key not in LAYER:
            sys.exit(f"crate {me} 没有在 LAYER 表里归层，请先补上")
        rows.append((key, sorted(map(short, normal)), sorted(map(short, dev))))
    extra = set(LAYER) - {r[0] for r in rows}
    if extra:
        sys.exit(f"LAYER 表里有工作区不存在的 crate：{sorted(extra)}")
    rows.sort(key=lambda r: layer_key(r[0]))
    return rows


def render(rows):
    lines = [
        "| crate | 层 | 正常依赖 | 仅 dev 依赖 |",
        "|---|---|---|---|",
    ]
    for name, normal, dev in rows:
        lines.append(
            "| {} | {} | {} | {} |".format(
                "**agent24d**" if name == "agent24d" else name,
                LAYER[name],
                ", ".join(normal) if normal else "（无）",
                ", ".join(dev) if dev else "—",
            )
        )
    lines.append("")
    lines.append(f"共 {len(rows)} 个 crate（`cargo metadata` 的 workspace member 数）。")
    return "\n".join(lines)


def current_block(text):
    if BEGIN not in text or END not in text:
        sys.exit("文档里找不到生成标记")
    head, rest = text.split(BEGIN, 1)
    body, tail = rest.split(END, 1)
    return head, body.strip("\n"), tail


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    table = render(build_rows(load()))
    if mode == "":
        print(table)
        return
    with open(DOC, encoding="utf-8") as f:
        text = f.read()
    head, body, tail = current_block(text)
    if mode == "--check":
        if body != table:
            print("docs/ARCHITECTURE-LAYERS.md 的依赖表与 cargo metadata 不一致；"
                  "运行 python3 scripts/gen-layer-deps.py --write 重新生成", file=sys.stderr)
            sys.exit(1)
        print("依赖表与 cargo metadata 一致")
    elif mode == "--write":
        with open(DOC, "w", encoding="utf-8") as f:
            f.write(head + BEGIN + "\n" + table + "\n" + END + tail)
        print("已重写", DOC)
    else:
        sys.exit("未知参数：" + mode)


if __name__ == "__main__":
    main()
