#!/usr/bin/env python3
"""从 logo-welcome.png 推导 iDoris 像素小人。

不插值。每个格子取不透明像素的众数颜色，再收成原图里真实出现过的调色板。
四种状态只在基础网格上盖一层 {x, y, color} 数据，不手改 SVG。

用法（仓库根目录或本文件所在仓库均可）:
    python3 tools/mascot/build_mascot.py
    python3 tools/mascot/build_mascot.py --compare   # 只把 40/48/64 对照写到 /tmp
"""

from __future__ import annotations

import json
import sys
from collections import Counter, deque
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "apps/desktop/src/renderer/assets/logo-welcome.png"
OUT = ROOT / "docs/design/assets/idoris-mascot"

# 人物包围盒大约 123×177。方格会把她压扁，所以按长边 40/48/64 比较，并保持宽高比。
# 长边 64 最清楚（约 44×64）。画布再留一圈给描边和状态标记。
CANVAS_W = 48
CANVAS_H = 66
CHAR_W = 44
CHAR_H = 64
ORIGIN = (2, 1)

ALPHA_MIN = 160
COVER = 0.34
OUTLINE_ALPHA = 0.7

# 这些 hex 都必须是 logo-welcome.png 里真实出现的像素，脚本启动时会核对。
PALETTE: dict[str, str] = {
    "black": "#000000",
    "ink": "#252525",
    "silver": "#A0A0A0",
    "white": "#FEFEFE",
    "skin": "#F9D7B9",
    "skin_deep": "#F3C59E",
    "blush": "#E79B75",
    "orange": "#F77315",
}

ROLES: dict[str, str] = {
    "black": "瞳孔、眉毛、最深的缝",
    "ink": "头发和羽绒服（原图里出现最多的深色）",
    "silver": "袖标金属边、帽檐灰",
    "white": "兜帽、内衬、眼白、胸口标签",
    "skin": "皮肤亮部",
    "skin_deep": "皮肤暗部",
    "blush": "腮红和嘴",
    "orange": "拉链、发绳、袖口、漂浮像素、状态标记",
}

DARK = {"black", "ink"}

STATE_TITLE = {
    "idle": "iDoris 待机",
    "thinking": "iDoris 思考",
    "success": "iDoris 成功",
    "error": "iDoris 出错",
}
STATE_GROUP = {
    "idle": "idoris-state-idle",
    "thinking": "idoris-state-thinking",
    "success": "idoris-state-success",
    "error": "idoris-state-error",
}


def hex_to_rgb(value: str) -> tuple[int, int, int]:
    value = value.removeprefix("#")
    return (int(value[0:2], 16), int(value[2:4], 16), int(value[4:6], 16))


def rgb_to_hex(rgb: tuple[int, int, int]) -> str:
    return "#{:02X}{:02X}{:02X}".format(*rgb)


def classify(r: int, g: int, b: int) -> str:
    """先分色族，再落到调色板。避免用距离把橙色吸进皮肤。"""
    chroma = max(r, g, b) - min(r, g, b)
    lum = (r + g + b) / 3
    if r > 180 and g > 50 and b < 110 and r > b + 70 and g > b + 10 and r > g:
        return "orange"
    if (
        r > 190
        and 120 <= g <= 185
        and 90 <= b <= 165
        and r > g + 25
        and abs(g - b) < 55
        and lum < 215
    ):
        return "blush"
    if r > 200 and g > 155 and b > 125 and r > b + 20 and g > b and chroma > 18 and lum >= 175:
        return "skin" if lum >= 208 else "skin_deep"
    if chroma < 28:
        if lum >= 220:
            return "white"
        if lum >= 120:
            return "silver"
        if lum < 12:
            return "black"
        return "ink"
    if lum < 40:
        return "black"
    if r > g > b and lum >= 140:
        return "skin_deep"
    return "ink"


def load_source() -> Image.Image:
    image = Image.open(SRC).convert("RGBA")
    pixels = image.load()
    width, height = image.size
    present: set[tuple[int, int, int]] = set()
    for y in range(height):
        for x in range(width):
            r, g, b, a = pixels[x, y]
            if a >= ALPHA_MIN:
                present.add((r, g, b))
            # 半透明的白边是抠白底留下的，收成全透明。不透明的白兜帽保留。
            if 0 < a < ALPHA_MIN:
                pixels[x, y] = (r, g, b, 0)
    missing = [name for name, hex_value in PALETTE.items() if hex_to_rgb(hex_value) not in present]
    if missing:
        raise SystemExit(f"调色板颜色不在原图里: {missing}")
    return image


def body_bbox(image: Image.Image) -> tuple[int, int, int, int]:
    """最大的不透明连通块。漂着的橙色点不拿来撑包围盒。"""
    pixels = image.load()
    width, height = image.size
    seen = [[False] * width for _ in range(height)]
    best: list[tuple[int, int]] = []
    for y in range(height):
        for x in range(width):
            if seen[y][x] or pixels[x, y][3] < ALPHA_MIN:
                continue
            queue = deque([(x, y)])
            seen[y][x] = True
            comp: list[tuple[int, int]] = []
            while queue:
                cx, cy = queue.popleft()
                comp.append((cx, cy))
                for dy in (-1, 0, 1):
                    for dx in (-1, 0, 1):
                        if dx == 0 and dy == 0:
                            continue
                        nx, ny = cx + dx, cy + dy
                        if nx < 0 or ny < 0 or nx >= width or ny >= height or seen[ny][nx]:
                            continue
                        if pixels[nx, ny][3] < ALPHA_MIN:
                            continue
                        seen[ny][nx] = True
                        queue.append((nx, ny))
            if len(comp) > len(best):
                best = comp
    xs = [p[0] for p in best]
    ys = [p[1] for p in best]
    return min(xs), min(ys), max(xs), max(ys)


def downsample(image: Image.Image, box: tuple[int, int, int, int], gw: int, gh: int) -> list[list[str | None]]:
    crop = image.crop((box[0], box[1], box[2] + 1, box[3] + 1))
    pixels = crop.load()
    width, height = crop.size
    grid: list[list[str | None]] = [[None] * gw for _ in range(gh)]
    for gy in range(gh):
        y0 = gy * height // gh
        y1 = max(y0 + 1, (gy + 1) * height // gh)
        for gx in range(gw):
            x0 = gx * width // gw
            x1 = max(x0 + 1, (gx + 1) * width // gw)
            counts: Counter[str] = Counter()
            total = 0
            for y in range(y0, y1):
                for x in range(x0, x1):
                    total += 1
                    r, g, b, a = pixels[x, y]
                    if a < ALPHA_MIN:
                        continue
                    counts[classify(r, g, b)] += 1
            opaque = sum(counts.values())
            if total == 0 or opaque / total < COVER:
                continue
            grid[gy][gx] = counts.most_common(1)[0][0]
    return grid


def place(char: list[list[str | None]], origin: tuple[int, int], cw: int, ch: int) -> list[list[str | None]]:
    canvas = [[None] * cw for _ in range(ch)]
    ox, oy = origin
    for y, row in enumerate(char):
        for x, color in enumerate(row):
            canvas[oy + y][ox + x] = color
    return canvas


def neighbors(x: int, y: int, w: int, h: int, eight: bool = True) -> list[tuple[int, int]]:
    found = []
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            if dx == 0 and dy == 0:
                continue
            if not eight and dx != 0 and dy != 0:
                continue
            nx, ny = x + dx, y + dy
            if 0 <= nx < w and 0 <= ny < h:
                found.append((nx, ny))
    return found


def simplify_chest(grid: list[list[str | None]]) -> None:
    """胸口的「iDORIS」在 44px 宽里是一堆碎银点。抹掉后改成小白标签加一颗橙点。
    右袖上同一高度的银点收成 AI 徽章方框。脸和白色内衬不动。
    """
    h = len(grid)
    w = len(grid[0])
    text: list[tuple[int, int]] = []
    badge: list[tuple[int, int]] = []
    for y in range(46, 54):
        for x in range(20, 33):
            if grid[y][x] == "silver":
                text.append((x, y))
        for x in range(33, w):
            if grid[y][x] == "silver":
                badge.append((x, y))
    for x, y in text:
        grid[y][x] = "ink"
    for y in range(50, 52):
        for x in range(23, 27):
            if 0 <= x < w and 0 <= y < h:
                grid[y][x] = "white"
    if 0 <= 22 < w:
        grid[50][22] = "orange"
    if badge:
        cx = round(sum(p[0] for p in badge) / len(badge))
        cy = round(sum(p[1] for p in badge) / len(badge))
    else:
        cx, cy = 38, 50
    x0 = min(max(0, cx - 2), w - 4)
    y0 = min(max(0, cy - 2), h - 4)
    for y in range(y0, y0 + 4):
        for x in range(x0, x0 + 4):
            edge = x in (x0, x0 + 3) or y in (y0, y0 + 3)
            grid[y][x] = "white" if edge else "ink"


def stamp_sparkles(image: Image.Image, body: tuple[int, int, int, int], grid: list[list[str | None]]) -> None:
    """身体旁边的橙色碎点，各收成 1 格，不让它们把包围盒撑大。"""
    pixels = image.load()
    width, height = image.size
    x0, y0, x1, y1 = body
    seen = [[False] * width for _ in range(height)]
    sparkles = []
    for y in range(height):
        for x in range(width):
            if seen[y][x] or pixels[x, y][3] < ALPHA_MIN:
                continue
            r, g, b, a = pixels[x, y]
            if classify(r, g, b) != "orange":
                continue
            queue = deque([(x, y)])
            seen[y][x] = True
            comp = []
            while queue:
                cx, cy = queue.popleft()
                comp.append((cx, cy))
                for dy in (-1, 0, 1):
                    for dx in (-1, 0, 1):
                        if dx == 0 and dy == 0:
                            continue
                        nx, ny = cx + dx, cy + dy
                        if nx < 0 or ny < 0 or nx >= width or ny >= height or seen[ny][nx]:
                            continue
                        rr, gg, bb, aa = pixels[nx, ny]
                        if aa < ALPHA_MIN or classify(rr, gg, bb) != "orange":
                            continue
                        seen[ny][nx] = True
                        queue.append((nx, ny))
            if len(comp) > 16:
                continue
            cx = sum(p[0] for p in comp) / len(comp)
            cy = sum(p[1] for p in comp) / len(comp)
            if x0 - 4 <= cx <= x1 + 4 and y0 - 4 <= cy <= y1 + 4:
                continue
            near = x0 - 28 <= cx <= x1 + 28 and y0 - 28 <= cy <= y1 + 28
            if near:
                sparkles.append((len(comp), cx, cy))
    sparkles.sort(reverse=True)
    bw = max(1, x1 - x0)
    bh = max(1, y1 - y0)
    ox, oy = ORIGIN
    h = len(grid)
    w = len(grid[0])
    placed = 0
    for _, cx, cy in sparkles:
        if placed >= 4:
            break
        fx = ox + (cx - x0) / bw * (CHAR_W - 1)
        fy = oy + (cy - y0) / bh * (CHAR_H - 1)
        gx = round(fx)
        gy = round(fy)
        if gx < ox:
            gx = ox - 1
        elif gx >= ox + CHAR_W:
            gx = ox + CHAR_W
        if gy < oy:
            gy = oy - 1
        elif gy >= oy + CHAR_H:
            gy = oy + CHAR_H
        gx = max(0, min(w - 1, gx))
        gy = max(0, min(h - 1, gy))
        if grid[gy][gx] is None:
            grid[gy][gx] = "orange"
            placed += 1


def outline_cells(grid: list[list[str | None]]) -> list[tuple[int, int]]:
    """深色块外侧 1px。浅色本身在深底上就分得开，不再加一圈。"""
    h = len(grid)
    w = len(grid[0])
    cells = []
    for y in range(h):
        for x in range(w):
            if grid[y][x] is not None:
                continue
            if any(grid[ny][nx] in DARK for nx, ny in neighbors(x, y, w, h, eight=False)):
                cells.append((x, y))
    return cells


def paint(grid: list[list[str | None]], edits: list[dict[str, object]]) -> list[list[str | None]]:
    out = [row[:] for row in grid]
    h = len(out)
    w = len(out[0])
    for edit in edits:
        x = int(edit["x"])
        y = int(edit["y"])
        if 0 <= x < w and 0 <= y < h:
            color = edit["color"]
            out[y][x] = None if color is None else str(color)
    return out


def at(x: int, y: int, color: str) -> dict[str, object]:
    """人物网格坐标换成画布坐标。覆盖层只写这种 {x, y, color}。"""
    return {"x": x + ORIGIN[0], "y": y + ORIGIN[1], "color": color}


def build_overlays() -> dict[str, list[dict[str, object]]]:
    """四种状态。坐标是降采样网格上量出来的，只盖住眼睛、嘴和几个标记。

    左眼在 (11..13, 24..26)，右眼在 (20..24, 23..25)，嘴的弯在 y=33..34。
    马尾在画面右侧，所以星星和三个点放在左上方的空位。
    """
    thinking = [
        {"x": 16, "y": 0, "color": "orange"},
        {"x": 20, "y": 0, "color": "orange"},
        {"x": 24, "y": 0, "color": "orange"},
        # 左眼半闭
        at(11, 24, "black"),
        at(12, 24, "black"),
        at(13, 24, "black"),
        at(11, 25, "skin"),
        at(12, 25, "skin"),
        at(13, 25, "skin"),
        at(11, 26, "skin"),
        # 右眼半闭
        at(20, 24, "black"),
        at(21, 24, "black"),
        at(22, 24, "black"),
        at(23, 24, "black"),
        at(24, 24, "black"),
        at(20, 25, "skin"),
    ]
    success = [
        # 左笑眼
        at(11, 24, "black"),
        at(12, 24, "skin"),
        at(13, 24, "black"),
        at(11, 25, "skin"),
        at(12, 25, "black"),
        at(13, 25, "skin"),
        at(11, 26, "skin"),
        # 右笑眼
        at(20, 23, "black"),
        at(24, 23, "black"),
        at(20, 24, "skin"),
        at(21, 24, "skin"),
        at(22, 24, "black"),
        at(23, 24, "skin"),
        at(24, 24, "skin"),
        at(20, 25, "skin"),
        # 左上角的橙色星星。马尾占着右上角。
        {"x": 1, "y": 0, "color": "orange"},
        {"x": 0, "y": 1, "color": "orange"},
        {"x": 1, "y": 1, "color": "orange"},
        {"x": 2, "y": 1, "color": "orange"},
        {"x": 1, "y": 2, "color": "orange"},
    ]
    error = [
        # 眉头往鼻子那边压
        at(13, 22, "black"),
        at(14, 22, "black"),
        at(15, 23, "black"),
        at(23, 22, "black"),
        at(24, 22, "black"),
        at(19, 24, "black"),
        # 弯嘴改成平的
        at(21, 34, "skin"),
        at(22, 34, "skin"),
        at(20, 33, "ink"),
        at(21, 33, "ink"),
        at(22, 33, "ink"),
        at(23, 33, "ink"),
        at(24, 33, "ink"),
        # 左侧的橙色感叹号
        {"x": 0, "y": 24, "color": "orange"},
        {"x": 0, "y": 25, "color": "orange"},
        {"x": 0, "y": 27, "color": "orange"},
    ]
    return {"idle": [], "thinking": thinking, "success": success, "error": error}


def merge_runs(cells: dict[tuple[int, int], str]) -> list[tuple[int, int, int, int, str]]:
    if not cells:
        return []
    height = max(y for _, y in cells) + 1
    width = max(x for x, _ in cells) + 1
    runs: list[list[int | str]] = []
    for y in range(height):
        x = 0
        while x < width:
            color = cells.get((x, y))
            if color is None:
                x += 1
                continue
            x2 = x
            while cells.get((x2 + 1, y)) == color:
                x2 += 1
            runs.append([x, y, x2 - x + 1, 1, color])
            x = x2 + 1
    rects = []
    used = [False] * len(runs)
    for i, run in enumerate(runs):
        if used[i]:
            continue
        x, y, rw, rh, color = run
        while True:
            found = None
            for j in range(i + 1, len(runs)):
                if used[j]:
                    continue
                x2, y2, rw2, _rh2, color2 = runs[j]
                if y2 > y + rh:
                    break
                if y2 == y + rh and x2 == x and rw2 == rw and color2 == color:
                    found = j
                    break
            if found is None:
                break
            used[found] = True
            rh += 1
        rects.append((int(x), int(y), int(rw), int(rh), str(color)))
    return rects


def svg_for(name: str, grid: list[list[str | None]], rim: list[tuple[int, int]]) -> str:
    h = len(grid)
    w = len(grid[0])
    colored: dict[tuple[int, int], str] = {}
    for y in range(h):
        for x in range(w):
            if grid[y][x] is not None:
                colored[(x, y)] = PALETTE[grid[y][x]]
    body_rects = merge_runs(colored)
    rim_rects = merge_runs({(x, y): "#FFFFFF" for x, y in rim})
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" '
            f'width="{w}" height="{h}" shape-rendering="crispEdges" role="img">'
        ),
        f"  <title>{STATE_TITLE[name]}</title>",
        f'  <g id="idoris-outline" fill="#FFFFFF" fill-opacity="{OUTLINE_ALPHA}">',
    ]
    for x, y, rw, rh, _color in rim_rects:
        lines.append(f'    <rect x="{x}" y="{y}" width="{rw}" height="{rh}"/>')
    lines.append("  </g>")
    lines.append(f'  <g id="{STATE_GROUP[name]}">')
    by_color: dict[str, list[tuple[int, int, int, int]]] = {}
    for x, y, rw, rh, color in body_rects:
        by_color.setdefault(color, []).append((x, y, rw, rh))
    for color in sorted(by_color):
        lines.append(f'    <g fill="{color}">')
        for x, y, rw, rh in by_color[color]:
            lines.append(f'      <rect x="{x}" y="{y}" width="{rw}" height="{rh}"/>')
        lines.append("    </g>")
    lines.append("  </g>")
    lines.append("</svg>")
    lines.append("")
    return "\n".join(lines)


def raster_svg_rects(svg: str) -> dict[tuple[int, int], str]:
    """核对状态组里的矩形没有盖错、没有重叠。描边层不参与。"""
    import re

    covered: dict[tuple[int, int], str] = {}
    fill = None
    in_state = False
    pattern = re.compile(
        r'<g id="idoris-state-|<g fill="(#[0-9A-F]+)"|<rect x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)"/>'
    )
    for match in pattern.finditer(svg):
        if match.group(0).startswith('<g id="idoris-state-'):
            in_state = True
            fill = None
            continue
        if match.group(1):
            fill = match.group(1)
            continue
        if not in_state or fill is None or match.group(2) is None:
            continue
        x, y, rw, rh = (int(match.group(i)) for i in range(2, 6))
        for yy in range(y, y + rh):
            for xx in range(x, x + rw):
                if (xx, yy) in covered:
                    raise SystemExit(f"SVG 矩形重叠: {(xx, yy)}")
                covered[(xx, yy)] = fill
    return covered


def assert_svg_matches(name: str, svg: str, grid: list[list[str | None]]) -> None:
    covered = raster_svg_rects(svg)
    for y, row in enumerate(grid):
        for x, color in enumerate(row):
            if color is None:
                if (x, y) in covered:
                    raise SystemExit(f"{name} 透明格被画上了 {(x, y)}")
                continue
            expected = PALETTE[color]
            got = covered.get((x, y))
            if got != expected:
                raise SystemExit(f"{name} 格子 ({x},{y}) 期望 {expected} 得到 {got}")


def render_sheet(states: list[tuple[str, list[list[str | None]]]], rims: list[list[tuple[int, int]]], bg: tuple[int, int, int], scale: int = 4) -> Image.Image:
    gap = 6
    pad = 8
    label_h = 16
    sprite_w = CANVAS_W * scale
    sprite_h = CANVAS_H * scale
    width = pad * 2 + len(states) * sprite_w + (len(states) - 1) * gap
    height = pad * 2 + sprite_h + label_h
    image = Image.new("RGBA", (width, height), bg + (255,))
    draw = ImageDraw.Draw(image)
    outline_rgb = (255, 255, 255, int(OUTLINE_ALPHA * 255))
    for index, (name, grid) in enumerate(states):
        ox = pad + index * (sprite_w + gap)
        oy = pad
        rim = set(rims[index])
        for y in range(CANVAS_H):
            for x in range(CANVAS_W):
                rect = [ox + x * scale, oy + y * scale, ox + (x + 1) * scale - 1, oy + (y + 1) * scale - 1]
                if (x, y) in rim:
                    draw.rectangle(rect, fill=outline_rgb)
                color = grid[y][x]
                if color is None:
                    continue
                draw.rectangle(rect, fill=hex_to_rgb(PALETTE[color]) + (255,))
        caption = {"idle": "idle", "thinking": "thinking", "success": "success", "error": "error"}[name]
        draw.text((ox, oy + sprite_h + 2), caption, fill=(40, 40, 40, 255) if bg[0] > 200 else (230, 230, 230, 255))
    return image


def build_base(image: Image.Image) -> list[list[str | None]]:
    box = body_bbox(image)
    char = downsample(image, box, CHAR_W, CHAR_H)
    simplify_chest(char)
    grid = place(char, ORIGIN, CANVAS_W, CANVAS_H)
    stamp_sparkles(image, box, grid)
    return grid


def compare(image: Image.Image) -> None:
    box = body_bbox(image)
    bw = box[2] - box[0] + 1
    bh = box[3] - box[1] + 1
    dest = Path("/tmp/mascot-compare")
    dest.mkdir(parents=True, exist_ok=True)
    print(f"body bbox {box} size {bw}x{bh}")
    for long_side in (40, 48, 64):
        gh = long_side
        gw = max(1, round(long_side * bw / bh))
        grid = downsample(image, box, gw, gh)
        scale = 8 if long_side < 64 else 6
        for bg, label in ((((255, 255, 255), "light"), ((17, 19, 24), "dark"))):
            image_out = Image.new("RGBA", (gw * scale, gh * scale), bg + (255,))
            draw = ImageDraw.Draw(image_out)
            for y, row in enumerate(grid):
                for x, color in enumerate(row):
                    if color is None:
                        continue
                    draw.rectangle(
                        [x * scale, y * scale, (x + 1) * scale - 1, (y + 1) * scale - 1],
                        fill=hex_to_rgb(PALETTE[color]) + (255,),
                    )
            path = dest / f"long{long_side}-{gw}x{gh}-{label}.png"
            image_out.save(path)
            print("wrote", path)


def write_outputs(image: Image.Image) -> None:
    base = build_base(image)
    overlays = build_overlays()
    OUT.mkdir(parents=True, exist_ok=True)
    states = []
    rims = []
    for name in ("idle", "thinking", "success", "error"):
        grid = paint(base, overlays[name])
        rim = outline_cells(grid)
        svg = svg_for(name, grid, rim)
        assert_svg_matches(name, svg, grid)
        (OUT / f"{name}.svg").write_text(svg, encoding="utf-8")
        states.append((name, grid))
        rims.append(rim)
    counts: Counter[str] = Counter(color for row in base for color in row if color)
    palette = {
        "source": "apps/desktop/src/renderer/assets/logo-welcome.png",
        "grid": {"width": CANVAS_W, "height": CANVAS_H},
        "character": {"width": CHAR_W, "height": CHAR_H, "x": ORIGIN[0], "y": ORIGIN[1]},
        "sampling": "每个格子取不透明像素的众数，再归入原图里真实出现过的颜色",
        "outline": {
            "hex": "#FFFFFF",
            "opacity": OUTLINE_ALPHA,
            "note": "深色轮廓外侧 1px 半透明白，浅底上几乎看不见，深底上把黑发和羽绒服分开",
        },
        "colors": [
            {"id": name, "hex": PALETTE[name], "role": ROLES[name], "cells": counts.get(name, 0)}
            for name in PALETTE
        ],
        "overlays": overlays,
    }
    (OUT / "palette.json").write_text(json.dumps(palette, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    render_sheet(states, rims, (255, 255, 255)).save(OUT / "preview-light.png")
    render_sheet(states, rims, (0x11, 0x13, 0x18)).save(OUT / "preview-dark.png")
    print(f"grid {CANVAS_W}x{CANVAS_H} character {CHAR_W}x{CHAR_H} at {ORIGIN}")
    print("colors", len(PALETTE), "base cells", sum(counts.values()))
    for name in PALETTE:
        print(f"  {name:10} {PALETTE[name]} {counts.get(name, 0)}")


def main() -> None:
    image = load_source()
    if "--compare" in sys.argv:
        compare(image)
        return
    write_outputs(image)


if __name__ == "__main__":
    main()
