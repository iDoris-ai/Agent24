#!/usr/bin/env python3
"""菌丝节点 SVG 生成器（docs/brand/VI.md §3）。改参数后重跑：
   python3 docs/brand/src/gen-svg.py && bash docs/brand/src/render.sh

琥珀核心 = 本机 Agent，核心里写「24」；三条菌丝连到三个节点（外壳与其他 Agent）。
32px 及以下数字会糊，用不带数字、线更粗的小尺寸版（app-icon-small.svg、主标、托盘）。"""
import math, pathlib
OUT = pathlib.Path(__file__).resolve().parent.parent
ANGLES = (330, 90, 210)

def pt(c, r, deg):
    a = math.radians(deg - 90)
    return c + r * math.cos(a), c + r * math.sin(a)

def hyphae(c, reach, line_w, node_r, core_r, line, node, amber, extra=""):
    ps = [pt(c, reach, a) for a in ANGLES]
    s = "".join(f'<line x1="{c}" y1="{c}" x2="{x:.2f}" y2="{y:.2f}" stroke="{line}" stroke-width="{line_w}" stroke-linecap="round"/>\n  ' for x, y in ps)
    s += "".join(f'<circle cx="{x:.2f}" cy="{y:.2f}" r="{node_r}" fill="{node}"/>\n  ' for x, y in ps)
    return s + extra + f'<circle cx="{c}" cy="{c}" r="{core_r}" fill="{amber}"/>'

def svg(size, body, defs=""):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">\n  {defs}{body}\n</svg>\n'

# 主标（侧栏等小尺寸，不带数字）
(OUT / "mark-dark.svg").write_text(svg(256, hyphae(128, 92, 14, 20, 38, "#8a8a80", "#F0F0E8", "#E8C547")))
(OUT / "mark-light.svg").write_text(svg(256, hyphae(128, 92, 14, 20, 38, "#888880", "#1A1A16", "#B89A10")))

# App 图标：macOS 1024 网格，824 圆角方块
DEFS = ('<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#24241f"/><stop offset="1" stop-color="#0b0b0a"/></linearGradient>'
        '<radialGradient id="glow"><stop offset="0" stop-color="#E8C547" stop-opacity=".5"/><stop offset="1" stop-color="#E8C547" stop-opacity="0"/></radialGradient></defs>\n  '
        '<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#g)"/>\n  '
        '<rect x="101" y="101" width="822" height="822" rx="184" fill="none" stroke="#3a3a33" stroke-width="2"/>\n  ')
GLOW = '<circle cx="512" cy="512" r="{r}" fill="url(#glow)"/>\n  '
TEXT = ('\n  <text x="508" y="566" text-anchor="middle" font-family="Menlo, \'SF Mono\', monospace" font-weight="700" '
        'font-size="150" letter-spacing="-8" fill="#16160f">24</text>')
(OUT / "app-icon.svg").write_text(svg(1024, hyphae(512, 264, 30, 46, 140, "#5a5a50", "#F0F0E8", "#E8C547", GLOW.format(r=230)) + TEXT, DEFS))
(OUT / "app-icon-small.svg").write_text(svg(1024, hyphae(512, 258, 46, 64, 112, "#6a6a60", "#F0F0E8", "#E8C547", GLOW.format(r=190)), DEFS))

# 托盘模板图（macOS Template Image：只用黑 + 透明度）。32px = 16pt@2x
(OUT / "tray-template.svg").write_text(svg(32, hyphae(16, 11, 2.2, 2.8, 4.6, "rgba(0,0,0,0.55)", "#000", "#000")))
print("ok")
