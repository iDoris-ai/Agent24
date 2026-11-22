#!/usr/bin/env python3
"""昼夜环 SVG 生成器（docs/brand/VI.md §3）。改参数后重跑：
   python3 docs/brand/src/gen-svg.py && bash docs/brand/src/render.sh

表盘式刻度：24 段 = 24 小时；0/6/12/18 时为长主刻度，其余为短刻度；
唯一点亮的一段（顶部）用琥珀色并加粗。避免 24 段等长时像「加载中」转圈。"""
import math, pathlib
OUT = pathlib.Path(__file__).resolve().parent.parent

def dial(c, R, minor_len, major_len, w_minor, w_major, w_lit, tick, lit, core, core_r):
    parts = []
    for i in range(24):
        a = i / 24 * 2 * math.pi - math.pi / 2
        is_lit, is_major = i == 0, i % 6 == 0
        ln = major_len if is_major else minor_len
        w = w_lit if is_lit else (w_major if is_major else w_minor)
        r = R - ln
        x1, y1 = c + r * math.cos(a), c + r * math.sin(a)
        x2, y2 = c + R * math.cos(a), c + R * math.sin(a)
        parts.append(f'<line x1="{x1:.2f}" y1="{y1:.2f}" x2="{x2:.2f}" y2="{y2:.2f}" '
                     f'stroke="{lit if is_lit else tick}" stroke-width="{w:.2f}" stroke-linecap="round"/>')
    parts.append(f'<circle cx="{c}" cy="{c}" r="{core_r:.2f}" fill="{core}"/>')
    return "\n  ".join(parts)

def svg(size, body, pre=""):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">\n  {pre}{body}\n</svg>\n'

def mark(tick, lit, core):  # 256 网格
    return dial(128, 114, 14, 40, 9, 12, 18, tick, lit, core, 22)

(OUT / "mark-dark.svg").write_text(svg(256, mark("#5a5a50", "#E8C547", "#F0F0E8")))
(OUT / "mark-light.svg").write_text(svg(256, mark("#b7b7ad", "#B89A10", "#1A1A16")))

# App 图标：macOS 1024 网格，824 圆角方块
bg = ('<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1">'
      '<stop offset="0" stop-color="#24241f"/><stop offset="1" stop-color="#0b0b0a"/></linearGradient></defs>\n  '
      '<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#g)"/>\n  '
      '<rect x="101" y="101" width="822" height="822" rx="184" fill="none" stroke="#3a3a33" stroke-width="2"/>\n  ')
(OUT / "app-icon.svg").write_text(svg(1024, dial(512, 316, 46, 120, 26, 34, 52, "#5a5a50", "#E8C547", "#F0F0E8", 58), bg))

# 托盘模板图（macOS Template Image：只用黑 + 透明度）。16pt 太小放不下 24 段，
# 用简化版：细圆环 + 点亮刻度 + 中心点（VI.md §3.2）。32px = 16pt@2x
def arc(c, r, deg_from, deg_to):
    p = lambda d: (c + r * math.cos(math.radians(d - 90)), c + r * math.sin(math.radians(d - 90)))
    (x1, y1), (x2, y2) = p(deg_from), p(deg_to)
    return f'M{x1:.2f} {y1:.2f} A{r} {r} 0 0 1 {x2:.2f} {y2:.2f}'

tray = ('<circle cx="16" cy="16" r="12.5" fill="none" stroke="#000" stroke-opacity="0.45" stroke-width="2"/>\n  '
        f'<path d="{arc(16, 12.5, -22, 22)}" fill="none" stroke="#000" stroke-width="4" stroke-linecap="round"/>\n  '
        '<circle cx="16" cy="16" r="3.6" fill="#000"/>')
(OUT / "tray-template.svg").write_text(svg(32, tray))
print("ok")
